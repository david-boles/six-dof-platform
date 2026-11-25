//! Browser UI for the six-DOF test stand.
//!
//! Data flow: Web Serial → postcard-rpc client (`serial.rs`) → subscription tasks →
//! plain (non-reactive) accumulators → copied into Leptos signals at a fixed, slow
//! rate for display. 250 Hz readings never go through the reactive system directly.

mod serial;

use std::{cell::RefCell, rc::Rc, time::Duration};

use leptos::{prelude::*, task::spawn_local};
use postcard_rpc::{
    host_client::{HostErr, MultiSubRxError},
    standard_icd::{LoggingTopic, WireError},
};
use six_dof_platform_common::interface::{
    AdcStatus, AdcStatusTopic, CHANNELS, DeviceInfo, GetAdcStatusEndpoint, GetCalibrationEndpoint,
    GetInfoEndpoint, GetInterfaceHashEndpoint, GetTimeEndpoint, GetZeroEndpoint, INTERFACE_HASH,
    Readings, ReadingsTopic, WRENCH_LEN, Wrench, WrenchTopic,
};

use crate::serial::Connection;

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

/// Whether the device still uses its boot defaults (read once after connecting;
/// only this UI can change them, and it doesn't yet).
#[derive(Clone, Copy, Debug, PartialEq)]
struct CalibrationFlags {
    matrix_is_boot_default: bool,
    zero_is_boot_default: bool,
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
    let calibration_flags = RwSignal::new(None::<CalibrationFlags>);
    let log = RwSignal::new(Vec::<String>::new());

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
            calibration_flags.set(None);
            spawn_local(async move {
                match start(accum, adc_status, log).await {
                    Ok((conn, info, flags)) => {
                        let client = conn.client.clone();
                        *connection.borrow_mut() = Some(conn);
                        calibration_flags.set(Some(flags));
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

        <h2>"Wrench"</h2>
        <p>
            "External load on the platform, torques about the origin (center of the top \
             triangle). "
            {move || match calibration_flags.get() {
                None => String::new(),
                Some(f) => {
                    let matrix = if f.matrix_is_boot_default {
                        "nominal (datasheet scale, CAD geometry; not calibrated)"
                    } else {
                        "set by the UI"
                    };
                    let zero = if f.zero_is_boot_default {
                        "not zeroed"
                    } else {
                        "zeroed"
                    };
                    format!("Calibration: {matrix}. Zero: {zero}.")
                }
            }}
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
                                    readings
                                        .with(|r| {
                                            r.latest_wrench.as_ref().map(|w| {
                                                // Forces to 1 mN, torques to 0.1 mN·m.
                                                if i < 3 {
                                                    format!("{:.3}", w.values[i])
                                                } else {
                                                    format!("{:.4}", w.values[i])
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
) -> Result<(Connection, DeviceInfo, CalibrationFlags), String> {
    let conn = serial::connect().await.map_err(|e| e.to_string())?;
    match setup(&conn.client, accum, adc_status, log).await {
        Ok((info, flags)) => Ok((conn, info, flags)),
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
) -> Result<(DeviceInfo, CalibrationFlags), String> {
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
    let flags = CalibrationFlags {
        matrix_is_boot_default: calibration.is_boot_default,
        zero_is_boot_default: zero.is_boot_default,
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

    Ok((info, flags))
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
