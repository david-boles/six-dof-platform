//! Browser UI for the six-DOF test stand.
//!
//! Data flow: Web Serial → postcard-rpc client (`serial.rs`) → subscription tasks →
//! plain (non-reactive) accumulators → copied into Leptos signals at a fixed, slow
//! rate for display. 250 Hz readings never go through the reactive system directly.

mod model_format;
mod serial;
mod storage;
mod view3d;

use std::{cell::RefCell, rc::Rc, time::Duration};

use leptos::{prelude::*, task::spawn_local};
use postcard_rpc::{
    host_client::{HostErr, MultiSubRxError},
    standard_icd::{LoggingTopic, WireError},
};
use six_dof_platform_common::{
    geometry::{DEFAULT_C, wrench_about},
    interface::{
        AVERAGE_DURATION_MS, AdcStatus, AdcStatusTopic, AverageCounts, AverageEndpoint,
        AverageError, CHANNELS, CalibrationMatrix, DeviceInfo, GetAdcStatusEndpoint,
        GetCalibrationEndpoint, GetInfoEndpoint, GetInterfaceHashEndpoint, GetTimeEndpoint,
        GetZeroEndpoint, INTERFACE_HASH, Readings, ReadingsTopic, SetCalibrationEndpoint, SetError,
        SetResult, SetZeroEndpoint, WRENCH_LEN, Wrench, WrenchTopic, ZeroCounts,
    },
};
use web_sys::HtmlInputElement;

use crate::{
    serial::{Client, Connection},
    storage::SavedCalibration,
};

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(App);
}

/// How often the displayed numbers are refreshed.
const DISPLAY_PERIOD: Duration = Duration::from_millis(100);
/// Number of device log messages kept on screen.
const LOG_LEN: usize = 50;

#[derive(Clone, Debug, PartialEq)]
enum Status {
    Disconnected,
    Connecting,
    Connected(DeviceInfo),
    Failed(String),
}

/// The calibration matrix C and zero offsets z the device uses: read after
/// connecting, updated when this UI sets them (nothing else can).
#[derive(Clone, Copy, Debug, PartialEq)]
struct DeviceCalibration {
    matrix: CalibrationMatrix,
    zero: ZeroCounts,
}

impl DeviceCalibration {
    fn is_zeroed(&self) -> bool {
        self.zero != [0.0; CHANNELS]
    }

    /// What C is, for display.
    fn matrix_label(&self, saved: Option<&SavedCalibration>) -> &'static str {
        if saved.is_some_and(|s| s.matrix == self.matrix) {
            "the calibration saved in this browser"
        } else if self.matrix == DEFAULT_C {
            "nominal (datasheet scale and CAD geometry, not calibrated)"
        } else {
            "custom (not the one saved in this browser)"
        }
    }
}

/// The signals a calibration/zero action updates (signals are `Copy` handles).
#[derive(Clone, Copy)]
struct ActionSignals {
    /// True while a request runs (buttons disabled).
    busy: RwSignal<bool>,
    device_calibration: RwSignal<Option<DeviceCalibration>>,
    /// Outcome of the last action (Ok = info, Err = error).
    message: RwSignal<Option<Result<String, String>>>,
}

/// Runs a calibration/zero request in the background: marks the UI busy, shows
/// the outcome, and on success applies `update` to the known device state.
fn run_action<F, Fut>(
    signals: ActionSignals,
    client: Option<Client>,
    action: &'static str,
    request: F,
    update: impl FnOnce(&mut DeviceCalibration) + 'static,
) where
    F: FnOnce(Client) -> Fut + 'static,
    Fut: Future<Output = Result<(), String>> + 'static,
{
    let Some(client) = client else { return };
    signals.busy.set(true);
    spawn_local(async move {
        let result = request(client).await;
        if result.is_ok() {
            signals.device_calibration.update(|d| {
                if let Some(d) = d {
                    update(d);
                }
            });
        }
        let time = js_sys::Date::new_0().to_locale_time_string("en-GB");
        signals.message.set(Some(match result {
            Ok(()) => Ok(format!("{time}: {action}.")),
            Err(e) => Err(format!("{time}: {action} failed: {e}")),
        }));
        signals.busy.set(false);
    });
}

/// Sends a new calibration matrix.
async fn set_matrix(client: &Client, matrix: CalibrationMatrix) -> Result<(), String> {
    set_result(client.send_resp::<SetCalibrationEndpoint>(&matrix).await)
}

/// Sends new zero offsets.
async fn set_zero(client: &Client, zero: ZeroCounts) -> Result<(), String> {
    set_result(client.send_resp::<SetZeroEndpoint>(&zero).await)
}

fn set_result(result: Result<SetResult, HostErr<WireError>>) -> Result<(), String> {
    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(SetError::NotFinite)) => Err("the device refused it: invalid numbers".into()),
        Err(e) => Err(format!("request failed: {e:?}")),
    }
}

/// Labels and units of the wrench components, in `Wrench::values` order.
const WRENCH_LABELS: [&str; WRENCH_LEN] = [
    "Fx (N)",
    "Fy (N)",
    "Fz (N)",
    "Mx (N·m)",
    "My (N·m)",
    "Mz (N·m)",
];

/// Everything shown about the readings stream; a snapshot of `ReadingsAccum`.
#[derive(Clone, Debug, Default, PartialEq)]
struct ReadingsView {
    latest: Option<Readings>,
    latest_wrench: Option<Wrench>,
    received: u64,
    /// Readings taken before we connected (queued on the device while nobody was
    /// reading the port, sent first): skipped, so their gap isn't counted as missed.
    stale: u64,
    /// Readings missing between consecutive timestamps (dropped by the firmware or
    /// not taken during an ADC re-init), plus readings lost to lag in the client's
    /// own queue.
    missed: u64,
    rate_hz: f64,
}

/// Updated for every reading (250 Hz); not reactive.
#[derive(Default)]
struct ReadingsAccum {
    view: ReadingsView,
    /// `DeviceInfo::sample_period_us`; 0 = unknown (no gap detection).
    period_us: u64,
    /// Device time when we connected (`time/get`); older readings are stale.
    connected_at_us: u64,
    /// Count and time (ms) at the last rate update.
    rate_mark: Option<(u64, f64)>,
}

impl ReadingsAccum {
    fn new(sample_period_us: u32, connected_at_us: u64) -> Self {
        Self {
            period_us: sample_period_us.into(),
            connected_at_us,
            ..Default::default()
        }
    }

    fn push(&mut self, r: Readings) {
        if r.timestamp_us < self.connected_at_us {
            self.view.stale += 1;
            return;
        }
        if let Some(last) = &self.view.latest
            && self.period_us > 0
            && let Some(dt) = r.timestamp_us.checked_sub(last.timestamp_us)
        {
            // Number of periods between the two readings, rounded; 1 = no gap.
            // A gap is counted from 1.5 periods, so timing jitter doesn't count.
            let periods = (dt + self.period_us / 2) / self.period_us;
            self.view.missed += periods.saturating_sub(1);
        }
        self.view.received += 1;
        self.view.latest = Some(r);
    }

    fn push_wrench(&mut self, w: Wrench) {
        if w.timestamp_us >= self.connected_at_us {
            self.view.latest_wrench = Some(w);
        }
    }

    /// Called at `DISPLAY_PERIOD`; updates the rate estimate and returns a snapshot.
    fn snapshot(&mut self) -> ReadingsView {
        let now = js_sys::Date::now();
        match self.rate_mark {
            Some((count, t)) if now - t >= 1000.0 => {
                self.view.rate_hz = (self.view.received - count) as f64 * 1000.0 / (now - t);
                self.rate_mark = Some((self.view.received, now));
            }
            Some(_) => {}
            None => self.rate_mark = Some((self.view.received, now)),
        }
        self.view.clone()
    }
}

#[component]
fn App() -> impl IntoView {
    let status = RwSignal::new(Status::Disconnected);
    let readings = RwSignal::new(ReadingsView::default());
    let adc_status = RwSignal::new(None::<AdcStatus>);
    let device_calibration = RwSignal::new(None::<DeviceCalibration>);
    let log = RwSignal::new(Vec::<String>::new());

    // Kept in the browser (storage.rs).
    let saved = RwSignal::new(None::<SavedCalibration>);
    // Result of the last calibration/zero action (Ok = info, Err = error).
    let calibration_message = RwSignal::new(None::<Result<String, String>>);
    match storage::load_calibration() {
        Ok(s) => saved.set(s),
        Err(e) => calibration_message.set(Some(Err(e))),
    }
    let reference_point = RwSignal::new(storage::load_reference_point());
    // A calibration or zero request is running.
    let busy = RwSignal::new(false);
    // The user said "not now" to applying the saved calibration (this connection).
    let offer_dismissed = RwSignal::new(false);

    // Non-reactive state shared between event handlers and background tasks.
    // (`Rc<RefCell<..>>` is fine: the browser runs all of this on one thread.)
    let accum = Rc::new(RefCell::new(ReadingsAccum::default()));
    let connection: Rc<RefCell<Option<Connection>>> = Rc::new(RefCell::new(None));

    {
        let accum = accum.clone();
        set_interval(
            move || readings.set(accum.borrow_mut().snapshot()),
            DISPLAY_PERIOD,
        );
    }

    let on_connect = {
        let accum = accum.clone();
        let connection = connection.clone();
        move |_| {
            let accum = accum.clone();
            let connection = connection.clone();
            status.set(Status::Connecting);
            // The device may have been reset since the last connection.
            adc_status.set(None);
            device_calibration.set(None);
            offer_dismissed.set(false);
            spawn_local(async move {
                match start(accum, adc_status, log).await {
                    Ok((conn, info, calibration)) => {
                        let client = conn.client.clone();
                        *connection.borrow_mut() = Some(conn);
                        device_calibration.set(Some(calibration));
                        status.set(Status::Connected(info));
                        // Watch for the link dying (unplug, firmware reset, ...).
                        client.wait_closed().await;
                        // Bind first: in an `if let`, the `borrow_mut()` temporary would
                        // live through the body, i.e. across the `.await`.
                        let conn = connection.borrow_mut().take();
                        if let Some(conn) = conn {
                            conn.close().await;
                            status.set(Status::Failed("connection lost".into()));
                        }
                    }
                    Err(e) => status.set(Status::Failed(e)),
                }
            });
        }
    };

    let on_disconnect = {
        let connection = connection.clone();
        move |_| {
            // Taking the connection first tells the watcher above this was intentional.
            if let Some(conn) = connection.borrow_mut().take() {
                spawn_local(async move {
                    conn.close().await;
                    status.set(Status::Disconnected);
                });
            }
        }
    };

    let connected = move || matches!(status.get(), Status::Connected(_));

    // Latest `average` result (or error text) and whether one is running.
    let average = RwSignal::new(None::<Result<AverageCounts, String>>);
    let averaging = RwSignal::new(false);
    let on_average = {
        let connection = connection.clone();
        move |_| {
            let Some(client) = connection.borrow().as_ref().map(|c| c.client.clone()) else {
                return;
            };
            averaging.set(true);
            spawn_local(async move {
                let result = match client.send_resp::<AverageEndpoint>(&()).await {
                    Ok(Ok(counts)) => Ok(counts),
                    Ok(Err(AverageError::AdcError)) => {
                        Err("an ADC error occurred while averaging (see device log)".into())
                    }
                    Ok(Err(AverageError::Timeout)) => {
                        Err("not enough readings arrived (is sampling running?)".into())
                    }
                    Err(HostErr::Wire(WireError::FailedToSpawn)) => {
                        Err("an average is already running".into())
                    }
                    Err(e) => Err(format!("average request failed: {e:?}")),
                };
                average.set(Some(result));
                averaging.set(false);
            });
        }
    };

    // The current client, if connected. Through a `StoredValue` handle (`Copy`,
    // `Send`), because some views (e.g. inside `<Show>`) require `Send` closures,
    // and the `Rc` itself isn't.
    let connection_handle = StoredValue::new_local(connection.clone());
    let client =
        move || connection_handle.with_value(|c| c.borrow().as_ref().map(|c| c.client.clone()));

    let signals = ActionSignals {
        busy,
        device_calibration,
        message: calibration_message,
    };
    let on_set_zero = move |_| {
        let Some(Ok(a)) = average.get_untracked() else {
            return;
        };
        let zero = a.mean;
        run_action(
            signals,
            client(),
            "zero set to the average",
            move |c| async move { set_zero(&c, zero).await },
            move |d| d.zero = zero,
        );
    };
    let on_clear_zero = move |_| {
        run_action(
            signals,
            client(),
            "zero cleared",
            |c| async move { set_zero(&c, [0.0; CHANNELS]).await },
            |d| d.zero = [0.0; CHANNELS],
        );
    };
    let on_apply_saved = move |_| {
        let Some(s) = saved.get_untracked() else {
            return;
        };
        let matrix = s.matrix;
        run_action(
            signals,
            client(),
            "saved calibration applied to the device",
            move |c| async move { set_matrix(&c, matrix).await },
            move |d| d.matrix = matrix,
        );
    };
    let on_reset_nominal = move |_| {
        run_action(
            signals,
            client(),
            "device reset to the nominal calibration",
            |c| async move { set_matrix(&c, DEFAULT_C).await },
            |d| d.matrix = DEFAULT_C,
        );
    };

    let note = RwSignal::new(String::new());
    let on_save = move |_| {
        let Some(d) = device_calibration.get_untracked() else {
            return;
        };
        let new = SavedCalibration::new(d.matrix, note.get_untracked());
        calibration_message.set(Some(match storage::store_calibration(&new) {
            Ok(()) => {
                saved.set(Some(new));
                Ok("the device's calibration is now saved in this browser.".into())
            }
            Err(e) => Err(e),
        }));
    };
    let on_export = move |_| {
        let Some(s) = saved.get_untracked() else {
            return;
        };
        if let Err(e) = storage::download(&s.file_name(), &s.to_json()) {
            calibration_message.set(Some(Err(e)));
        }
    };
    let on_import = move |ev: leptos::ev::Event| {
        let input: HtmlInputElement = event_target(&ev);
        spawn_local(async move {
            let result = storage::read_selected_file(&input)
                .await
                .and_then(|text| SavedCalibration::from_json(&text))
                .and_then(|new| {
                    storage::store_calibration(&new)?;
                    let replaced = saved
                        .get_untracked()
                        .map(|old| format!(" (replaced the one saved {})", old.saved_at))
                        .unwrap_or_default();
                    let message = format!(
                        "imported the calibration saved {}{replaced}; not applied to the \
                         device yet.",
                        new.saved_at
                    );
                    saved.set(Some(new));
                    offer_dismissed.set(false);
                    Ok(message)
                });
            calibration_message.set(Some(result));
            // Allow importing the same file again.
            input.set_value("");
        });
    };

    // Show the offer when connected, a calibration is saved, the device isn't
    // using it and the user hasn't said "not now".
    let offer_apply = move || {
        let Some(s) = saved.get() else { return false };
        device_calibration
            .get()
            .is_some_and(|d| d.matrix != s.matrix && !offer_dismissed.get())
    };

    let set_reference = move |axis: usize| {
        move |ev: leptos::ev::Event| {
            let input: HtmlInputElement = event_target(&ev);
            match input.value().trim().parse::<f64>() {
                Ok(v) if v.is_finite() => {
                    reference_point.update(|p| p[axis] = v);
                    storage::store_reference_point(reference_point.get_untracked());
                }
                // Invalid input: show the current value again.
                _ => input.set_value(&reference_point.get_untracked()[axis].to_string()),
            }
        }
    };

    view! {
        <h1>"Six-DOF test stand"</h1>
        <p>
            <button on:click=on_connect disabled=move || connected() || status.get() == Status::Connecting>
                "Connect"
            </button>
            " "
            <button on:click=on_disconnect disabled=move || !connected()>"Disconnect"</button>
            " "
            {move || match status.get() {
                Status::Disconnected => "Disconnected".to_string(),
                Status::Connecting => "Connecting...".to_string(),
                Status::Connected(info) => format!(
                    "Connected: device {:016x}, sample period {} µs",
                    info.unique_id, info.sample_period_us,
                ),
                Status::Failed(e) => format!("Error: {e}"),
            }}
        </p>

        <p>
            "ADC status: "
            {move || match adc_status.get() {
                None => "-",
                Some(AdcStatus::Ok) => "OK",
                Some(AdcStatus::Errored) => "errors occurred, sampling continues (see device log)",
                Some(AdcStatus::Terminated) => "terminated after too many errors; reset the device",
            }}
        </p>

        <h2>"Raw readings (ADC counts)"</h2>
        <table>
            <tr>
                {(0..CHANNELS).map(|i| view! { <th>{format!("Cell {i}")}</th> }).collect_view()}
            </tr>
            <tr>
                {(0..CHANNELS)
                    .map(|i| {
                        view! {
                            <td style="text-align: right; font-family: monospace; padding: 0 1em">
                                {move || {
                                    readings
                                        .with(|r| r.latest.as_ref().map(|l| l.raw[i].to_string()))
                                        .unwrap_or_else(|| "-".into())
                                }}
                            </td>
                        }
                    })
                    .collect_view()}
            </tr>
        </table>
        <p>
            {move || {
                readings
                    .with(|r| {
                        format!(
                            "device time {} s, received {}, missed {}, rate {:.1} Hz \
                             (skipped {} queued from before connecting)",
                            r.latest
                                .as_ref()
                                .map(|l| format!("{:.3}", l.timestamp_us as f64 / 1e6))
                                .unwrap_or("-".into()),
                            r.received,
                            r.missed,
                            r.rate_hz,
                            r.stale,
                        )
                    })
            }}
        </p>
        <p>
            <button on:click=on_average disabled=move || !connected() || averaging.get()>
                {format!("Average {:.0} s", AVERAGE_DURATION_MS as f64 / 1000.0)}
            </button>
            " "
            {move || match average.get() {
                _ if averaging.get() => "averaging...".to_string(),
                None => String::new(),
                Some(Ok(a)) => format!("{} readings", a.n),
                Some(Err(e)) => format!("Error: {e}"),
            }}
        </p>
        {move || {
            average
                .get()
                .and_then(Result::ok)
                .map(|a| {
                    let row = |label: &'static str, values: [f32; CHANNELS], decimals: usize| {
                        view! {
                            <tr>
                                <th style="text-align: left">{label}</th>
                                {values
                                    .map(|v| {
                                        view! {
                                            <td style="text-align: right; font-family: monospace; padding: 0 1em">
                                                {format!("{v:.decimals$}")}
                                            </td>
                                        }
                                    })
                                    .collect_view()}
                            </tr>
                        }
                    };
                    view! {
                        <table>
                            <tr>
                                <th></th>
                                {(0..CHANNELS)
                                    .map(|i| view! { <th>{format!("Cell {i}")}</th> })
                                    .collect_view()}
                            </tr>
                            {row("mean", a.mean, 1)}
                            {row("std. dev.", a.std_dev, 2)}
                        </table>
                    }
                })
        }}
        <p>
            <button
                on:click=on_set_zero
                disabled=move || {
                    !connected() || busy.get() || averaging.get()
                        || !matches!(average.get(), Some(Ok(_)))
                }
            >
                "Set the average as zero"
            </button>
            " "
            <button on:click=on_clear_zero disabled=move || !connected() || busy.get()>
                "Clear zero"
            </button>
            " Zeroing subtracts the offsets in the device's wrench calculation; the raw \
             counts above stay raw."
        </p>

        <h2>"Wrench"</h2>
        <p>
            {move || {
                let [x, y, z] = reference_point.get();
                format!("External load on the platform; torques about ({x}, {y}, {z}) mm. ")
            }}
            {move || match device_calibration.get() {
                None => String::new(),
                Some(d) => {
                    let zero = if d.is_zeroed() { "zeroed" } else { "not zeroed" };
                    format!(
                        "Calibration: {}. Zero: {zero}.",
                        saved.with(|s| d.matrix_label(s.as_ref())),
                    )
                }
            }}
        </p>
        <p>
            "Reference point for torques (mm; origin = center of the top triangle, top \
             surface of the platform): "
            {["x", "y", "z"]
                .into_iter()
                .enumerate()
                .map(|(axis, label)| {
                    view! {
                        {format!(" {label} ")}
                        <input
                            type="number"
                            step="any"
                            style="width: 6em"
                            prop:value=move || reference_point.get()[axis].to_string()
                            on:change=set_reference(axis)
                        />
                    }
                })
                .collect_view()}
        </p>
        <table>
            <tr>
                {WRENCH_LABELS.map(|label| view! { <th>{label}</th> }).collect_view()}
            </tr>
            <tr>
                {(0..WRENCH_LEN)
                    .map(|i| {
                        view! {
                            <td style="text-align: right; font-family: monospace; padding: 0 1em">
                                {move || {
                                    let point = reference_point.get();
                                    readings
                                        .with(|r| {
                                            r.latest_wrench.as_ref().map(|w| {
                                                let value = wrench_about(&w.values, point)[i];
                                                // Forces to 1 mN, torques to 0.1 mN·m.
                                                if i < 3 {
                                                    format!("{value:.3}")
                                                } else {
                                                    format!("{value:.4}")
                                                }
                                            })
                                        })
                                        .unwrap_or_else(|| "-".into())
                                }}
                            </td>
                        }
                    })
                    .collect_view()}
            </tr>
        </table>

        <h2>"3D view"</h2>
        <view3d::View3d
            wrench={
                let accum = accum.clone();
                move || accum.borrow().view.latest_wrench.as_ref().map(|w| w.values)
            }
            reference_point=reference_point
        />

        <h2>"Calibration"</h2>
        <Show when=offer_apply>
            <p style="background: #ffd; padding: 0.5em">
                {move || {
                    saved
                        .get()
                        .map(|s| {
                            format!(
                                "This browser has a saved calibration ({}{}) that the device \
                                 isn't using. Apply it? ",
                                s.saved_at,
                                if s.note.is_empty() { String::new() } else { format!(": {}", s.note) },
                            )
                        })
                }}
                <button on:click=on_apply_saved disabled=move || busy.get()>"Apply"</button>
                " "
                <button on:click=move |_| offer_dismissed.set(true)>"Not now"</button>
            </p>
        </Show>
        <p>
            "Device: "
            {move || match device_calibration.get() {
                None => "not connected".to_string(),
                Some(d) => saved.with(|s| d.matrix_label(s.as_ref())).to_string(),
            }}
            ". The device keeps it only until it's reset."
        </p>
        <p>
            "Saved in this browser: "
            {move || match saved.get() {
                None => "none".to_string(),
                Some(s) if s.note.is_empty() => format!("saved {}", s.saved_at),
                Some(s) => format!("saved {}: {}", s.saved_at, s.note),
            }}
        </p>
        <p>
            <button
                on:click=on_apply_saved
                disabled=move || !connected() || busy.get() || saved.with(Option::is_none)
            >
                "Apply the saved calibration to the device"
            </button>
            " "
            <button on:click=on_reset_nominal disabled=move || !connected() || busy.get()>
                "Reset the device to nominal"
            </button>
        </p>
        <p>
            "Note: "
            <input
                type="text"
                style="width: 20em"
                placeholder="e.g. how it was made"
                prop:value=move || note.get()
                on:input=move |ev| note.set(event_target_value(&ev))
            />
            " "
            <button on:click=on_save disabled=move || device_calibration.with(Option::is_none)>
                "Save the device's calibration in this browser"
            </button>
        </p>
        <p>
            <button on:click=on_export disabled=move || saved.with(Option::is_none)>
                "Export the saved calibration (JSON)"
            </button>
            " Import (replaces the saved one): "
            <input type="file" accept=".json,application/json" on:change=on_import />
        </p>
        <p>
            {move || match calibration_message.get() {
                None => String::new(),
                Some(Ok(m)) => m,
                Some(Err(e)) => format!("Error: {e}"),
            }}
        </p>

        <h2>"Device log"</h2>
        <ul>
            {move || {
                log.get().into_iter().rev().map(|e| view! { <li>{e}</li> }).collect_view()
            }}
        </ul>
    }
}

/// Connects and runs `setup`; closes the port again if that fails, so Connect
/// can be retried (e.g. after reflashing the firmware).
async fn start(
    accum: Rc<RefCell<ReadingsAccum>>,
    adc_status: RwSignal<Option<AdcStatus>>,
    log: RwSignal<Vec<String>>,
) -> Result<(Connection, DeviceInfo, DeviceCalibration), String> {
    let conn = serial::connect().await.map_err(|e| e.to_string())?;
    match setup(&conn.client, accum, adc_status, log).await {
        Ok((info, calibration)) => Ok((conn, info, calibration)),
        Err(e) => {
            conn.close().await;
            Err(e)
        }
    }
}

/// Checks the interface version, starts the subscription tasks and fetches device
/// info, ADC status, device time and calibration state.
async fn setup(
    client: &serial::Client,
    accum: Rc<RefCell<ReadingsAccum>>,
    adc_status: RwSignal<Option<AdcStatus>>,
    log: RwSignal<Vec<String>>,
) -> Result<(DeviceInfo, DeviceCalibration), String> {
    let closed = |_| "connection closed".to_string();

    // First, so a firmware/UI mismatch gives a clear message instead of failing
    // requests or silently missing topic messages later.
    match client.send_resp::<GetInterfaceHashEndpoint>(&()).await {
        Ok(hash) if hash == INTERFACE_HASH => {}
        Ok(hash) => {
            return Err(format!(
                "the firmware and this UI were built from different interface versions \
                 (firmware {hash:016x}, UI {INTERFACE_HASH:016x}): reflash the firmware \
                 and/or rebuild the UI from the same source"
            ));
        }
        Err(HostErr::Wire(WireError::UnknownKey)) => {
            return Err("the firmware is older than the interface version check \
                        (it doesn't know `interface/hash/get`): reflash it"
                .into());
        }
        Err(e) => return Err(format!("interface version request failed: {e:?}")),
    }

    // Subscribe before asking for the current status, so a change in between
    // isn't missed.
    let mut readings_sub = client
        .subscribe_multi::<ReadingsTopic>(512)
        .await
        .map_err(closed)?;
    let mut status_sub = client
        .subscribe_multi::<AdcStatusTopic>(8)
        .await
        .map_err(closed)?;
    // Only the latest wrench is shown, so a small queue is enough.
    let mut wrench_sub = client
        .subscribe_multi::<WrenchTopic>(16)
        .await
        .map_err(closed)?;
    let mut log_sub = client
        .subscribe_multi::<LoggingTopic>(16)
        .await
        .map_err(closed)?;

    let info = client
        .send_resp::<GetInfoEndpoint>(&())
        .await
        .map_err(|e| format!("info request failed: {e:?}"))?;
    let status = client
        .send_resp::<GetAdcStatusEndpoint>(&())
        .await
        .map_err(|e| format!("ADC status request failed: {e:?}"))?;
    // A status update may already have arrived; `Terminated` is final and
    // `Errored` only follows `Ok`, so keep whichever is "worse".
    adc_status.update(|s| *s = Some(s.map_or(status, |s| worse(s, status))));
    // Readings taken before this moment were queued on the device while nobody
    // read the port; they arrive first and would look like a long gap (TODO 4.1.3).
    let connected_at_us = client
        .send_resp::<GetTimeEndpoint>(&())
        .await
        .map_err(|e| format!("time request failed: {e:?}"))?;
    let calibration = client
        .send_resp::<GetCalibrationEndpoint>(&())
        .await
        .map_err(|e| format!("calibration request failed: {e:?}"))?;
    let zero = client
        .send_resp::<GetZeroEndpoint>(&())
        .await
        .map_err(|e| format!("zero request failed: {e:?}"))?;
    let device_calibration = DeviceCalibration {
        matrix: calibration.matrix,
        zero: zero.counts,
    };

    *accum.borrow_mut() = ReadingsAccum::new(info.sample_period_us, connected_at_us);
    log.set(Vec::new());

    {
        let accum = accum.clone();
        spawn_local(async move {
            loop {
                match wrench_sub.recv().await {
                    Ok(w) => accum.borrow_mut().push_wrench(w),
                    // Only the latest matters; gaps are already counted for readings.
                    Err(MultiSubRxError::Lagged(_)) => {}
                    Err(MultiSubRxError::IoClosed) => break,
                }
            }
        });
    }

    spawn_local(async move {
        loop {
            match readings_sub.recv().await {
                Ok(r) => accum.borrow_mut().push(r),
                // Our own queue overflowed (page too busy); count as missed.
                Err(MultiSubRxError::Lagged(n)) => accum.borrow_mut().view.missed += n,
                Err(MultiSubRxError::IoClosed) => break,
            }
        }
    });

    spawn_local(async move {
        loop {
            match status_sub.recv().await {
                Ok(s) => adc_status.set(Some(s)),
                // Only the latest status matters; the next one will arrive.
                Err(MultiSubRxError::Lagged(_)) => {}
                Err(MultiSubRxError::IoClosed) => break,
            }
        }
    });

    spawn_local(async move {
        loop {
            let line = match log_sub.recv().await {
                Ok(text) => text,
                Err(MultiSubRxError::Lagged(n)) => format!("({n} log messages lost)"),
                Err(MultiSubRxError::IoClosed) => break,
            };
            let time = js_sys::Date::new_0().to_locale_time_string("en-GB");
            log.update(|log| {
                if log.len() == LOG_LEN {
                    log.remove(0);
                }
                log.push(format!("{time}: {line}"));
            });
        }
    });

    Ok((info, device_calibration))
}

/// The more severe of two statuses (`Ok` < `Errored` < `Terminated`).
fn worse(a: AdcStatus, b: AdcStatus) -> AdcStatus {
    let rank = |s| match s {
        AdcStatus::Ok => 0,
        AdcStatus::Errored => 1,
        AdcStatus::Terminated => 2,
    };
    if rank(a) >= rank(b) { a } else { b }
}
