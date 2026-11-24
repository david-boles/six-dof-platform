//! Browser UI for the six-DOF test stand.
//!
//! Data flow: Web Serial → postcard-rpc client (`serial.rs`) → subscription tasks →
//! plain (non-reactive) accumulators → copied into Leptos signals at a fixed, slow
//! rate for display. 250 Hz readings never go through the reactive system directly.

mod serial;

use std::{cell::RefCell, rc::Rc, time::Duration};

use leptos::{prelude::*, task::spawn_local};
use postcard_rpc::{host_client::MultiSubRxError, standard_icd::LoggingTopic};
use six_dof_platform_common::interface::{
    AdcStatus, AdcStatusTopic, CHANNELS, DeviceInfo, GetAdcStatusEndpoint, GetInfoEndpoint,
    Readings, ReadingsTopic,
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

/// Everything shown about the readings stream; a snapshot of `ReadingsAccum`.
#[derive(Clone, Debug, Default, PartialEq)]
struct ReadingsView {
    latest: Option<Readings>,
    received: u64,
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
    /// Count and time (ms) at the last rate update.
    rate_mark: Option<(u64, f64)>,
}

impl ReadingsAccum {
    fn new(sample_period_us: u32) -> Self {
        Self {
            period_us: sample_period_us.into(),
            ..Default::default()
        }
    }

    fn push(&mut self, r: Readings) {
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
            spawn_local(async move {
                match start(accum, adc_status, log).await {
                    Ok((conn, info)) => {
                        let client = conn.client.clone();
                        *connection.borrow_mut() = Some(conn);
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
                            "device time {} s, received {}, missed {}, rate {:.1} Hz",
                            r.latest
                                .as_ref()
                                .map(|l| format!("{:.3}", l.timestamp_us as f64 / 1e6))
                                .unwrap_or("-".into()),
                            r.received,
                            r.missed,
                            r.rate_hz,
                        )
                    })
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

/// Connects, starts the subscription tasks and fetches device info and ADC status.
async fn start(
    accum: Rc<RefCell<ReadingsAccum>>,
    adc_status: RwSignal<Option<AdcStatus>>,
    log: RwSignal<Vec<String>>,
) -> Result<(Connection, DeviceInfo), String> {
    let conn = serial::connect().await.map_err(|e| e.to_string())?;
    let client = &conn.client;
    let closed = |_| "connection closed".to_string();

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

    *accum.borrow_mut() = ReadingsAccum::new(info.sample_period_us);
    log.set(Vec::new());

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

    Ok((conn, info))
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
