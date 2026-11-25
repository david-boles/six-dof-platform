//! Communication with the web UI: a `postcard-rpc` server over the USB-Serial-JTAG
//! serial port, plus the tasks that forward ADC readings, status and errors to it.
//!
//! Data path:
//! ```text
//!                                       ,--(OUTGOING: readings)--.
//!                                       |--(WRENCHES: wrenches)--|
//! ADC ISR --(STREAM, run_adcs)--> pump -+--(STATUS_CHANGED)-------+--> publisher --> USB
//!                                       `--(ERRORS: error log)---'
//! ```
//! The pump never blocks: if a queue is full (USB is slow, or nobody is reading the
//! port), messages are dropped there. That keeps the ADC side from ever seeing a full
//! STREAM because of USB. Dropped readings show up on the UI side as gaps in
//! `Readings::timestamp_us`. The status is a "latest value" signal, so it is never lost.
//!
//! The pump also computes the wrench `w = C · (c − z)` for every set of readings,
//! with the calibration matrix C and zero offsets z in `CALIBRATION` (set by the UI
//! through the `calibration/set` and `zero/set` endpoints; RAM only), and feeds
//! `AVERAGING` while an `average` request runs (`average_handler`).

use core::{
    cell::{Cell, RefCell},
    fmt::Write,
};

use embassy_futures::select::{Either4, select4};
use embassy_sync::{
    blocking_mutex::{Mutex, raw::CriticalSectionRawMutex},
    channel::{Channel, Receiver},
    signal::Signal,
};
use embassy_sync_07::blocking_mutex::raw::CriticalSectionRawMutex as CriticalSectionRawMutex07;
use embassy_time::{Duration, Instant, with_timeout};
use esp_hal::{
    Async,
    usb::usb_serial_jtag::{UsbSerialJtagRx, UsbSerialJtagTx},
};
use postcard_rpc::{
    define_dispatch,
    header::VarHeader,
    server::{
        Sender, Server, SpawnContext,
        impls::embedded_io_async_v0_6::{
            EioWireTx, WireStorage,
            dispatch_impl::{WireRxBuf, WireRxImpl, WireSpawnImpl, spawn_fn},
        },
    },
};
use six_dof_platform_common::{
    averaging::CountsAccumulator,
    calibration::{all_finite, wrench_from_counts},
    geometry::DEFAULT_C,
    interface::{
        AVERAGE_DURATION_MS, AdcStatus, AdcStatusTopic, AverageEndpoint, AverageError,
        AverageResult, CHANNELS, CalibrationMatrix, CalibrationSetting, DeviceInfo, ENDPOINT_LIST,
        GetAdcStatusEndpoint, GetCalibrationEndpoint, GetInfoEndpoint, GetInterfaceHashEndpoint,
        GetTimeEndpoint, GetZeroEndpoint, INTERFACE_HASH, MAX_DEVICE_MESSAGE_SIZE, MAX_LOG_LEN,
        MAX_REQUEST_SIZE, Readings, ReadingsTopic, SetCalibrationEndpoint, SetError, SetResult,
        SetZeroEndpoint, TOPICS_IN_LIST, TOPICS_OUT_LIST, Wrench, WrenchTopic, ZeroCounts,
        ZeroSetting,
    },
};

use crate::input_adcs::{AdcsError, AdcsEvent, SAMPLE_PERIOD};

/// Depth of the ADC event queue between the sampling task and the pump.
pub const STREAM_DEPTH: usize = 16;
/// The queue the ADC sampling task (`run_adcs`) writes into.
pub type Stream = Channel<CriticalSectionRawMutex, AdcsEvent, STREAM_DEPTH>;

/// Readings waiting to be sent (16 = 64 ms of readings).
static OUTGOING: Channel<CriticalSectionRawMutex, Readings, 16> = Channel::new();
/// Wrenches waiting to be sent (one per set of readings, same depth).
static WRENCHES: Channel<CriticalSectionRawMutex, Wrench, 16> = Channel::new();
/// Errors waiting to be sent as log text. `run_adcs` terminates after two errors
/// within a second, so this can only overflow if USB is stalled for a long time.
static ERRORS: Channel<CriticalSectionRawMutex, AdcsError, 4> = Channel::new();
/// Current ADC status, read by the `adc/status/get` handler.
static ADC_STATUS: Mutex<CriticalSectionRawMutex, Cell<AdcStatus>> =
    Mutex::new(Cell::new(AdcStatus::Ok));
/// Wakes the publisher when the status changes. A `Signal` holds only the latest
/// value, so a change can't be dropped (only superseded).
static STATUS_CHANGED: Signal<CriticalSectionRawMutex, AdcStatus> = Signal::new();

/// Calibration in use for the wrench, `w = C · (c − z)`.
#[derive(Clone, Copy)]
struct CalibrationState {
    /// C
    matrix: CalibrationMatrix,
    matrix_is_boot_default: bool,
    /// z
    zero: ZeroCounts,
    zero_is_boot_default: bool,
}

/// Written by the `*/set` handlers, copied by the pump for every set of readings
/// (about 170 bytes; the copy happens inside a short critical section, the
/// computation outside it, so interrupts are only held off for the copy).
static CALIBRATION: Mutex<CriticalSectionRawMutex, Cell<CalibrationState>> =
    Mutex::new(Cell::new(CalibrationState {
        matrix: DEFAULT_C,
        matrix_is_boot_default: true,
        zero: [0.0; CHANNELS],
        zero_is_boot_default: true,
    }));

/// Number of readings `average` takes (250 = 1 s at 250 Hz).
const AVERAGE_READINGS: u32 =
    (AVERAGE_DURATION_MS as u64 * 1000 / SAMPLE_PERIOD.as_micros()) as u32;
/// How long `average_handler` waits before giving up (`AverageError::Timeout`).
const AVERAGE_TIMEOUT: Duration = Duration::from_millis(AVERAGE_DURATION_MS as u64 + 1000);
/// `Some` while an `average` request runs: the pump adds every set of readings.
static AVERAGING: Mutex<CriticalSectionRawMutex, RefCell<Option<CountsAccumulator>>> =
    Mutex::new(RefCell::new(None));
/// Set by the pump when the average is complete or failed.
static AVERAGE_DONE: Signal<CriticalSectionRawMutex, Result<CountsAccumulator, AverageError>> =
    Signal::new();

// ---------------------------------------------------------------------------
// postcard-rpc server types (same structure as postcard-rpc's esp32c6-serial example)
// ---------------------------------------------------------------------------

/// Buffer for one decoded incoming frame (header + request body), handed to
/// `Server::new`. Larger requests are dropped by the server.
pub const RX_FRAME_LEN: usize = MAX_REQUEST_SIZE;
/// Raw receive buffer: one COBS-encoded request including its terminating 0.
const RX_RAW_LEN: usize = cobs::max_encoding_length(MAX_REQUEST_SIZE) + 1;
/// Transmit buffer: one COBS-encoded device message including its terminating 0.
const TX_RAW_LEN: usize = cobs::max_encoding_length(MAX_DEVICE_MESSAGE_SIZE) + 1;

pub type Rx = UsbSerialJtagRx<'static, Async>;
pub type Tx = UsbSerialJtagTx<'static, Async>;
pub type Storage = WireStorage<Rx, Tx, CriticalSectionRawMutex07, RX_RAW_LEN, TX_RAW_LEN>;
pub type AppTx = EioWireTx<CriticalSectionRawMutex07, Tx>;
pub type AppRx = WireRxImpl<Rx>;
pub type AppServer = Server<AppTx, AppRx, WireRxBuf, App>;

/// Buffers and the transmit-side mutex. Initialized once in `main`.
pub static STORAGE: Storage = Storage::new();

/// State available to endpoint handlers.
pub struct Context {
    pub unique_id: u64,
}

/// What spawned handlers (`kind = spawn`) get instead of `&mut Context`: they
/// outlive the dispatch call, so they can't borrow it. `average_handler` needs
/// nothing from it.
impl SpawnContext for Context {
    type SpawnCtxt = ();
    fn spawn_ctxt(&mut self) -> Self::SpawnCtxt {}
}

define_dispatch! {
    app: App;
    spawn_fn: spawn_fn;
    tx_impl: AppTx;
    spawn_impl: WireSpawnImpl;
    context: Context;

    endpoints: {
        list: ENDPOINT_LIST;

        | EndpointTy               | kind     | handler                 |
        | ----------               | ----     | -------                 |
        | GetInterfaceHashEndpoint | blocking | interface_hash_handler  |
        | GetInfoEndpoint          | blocking | info_handler            |
        | GetAdcStatusEndpoint     | blocking | adc_status_handler      |
        | GetTimeEndpoint          | blocking | time_handler            |
        | GetCalibrationEndpoint   | blocking | get_calibration_handler |
        | SetCalibrationEndpoint   | blocking | set_calibration_handler |
        | GetZeroEndpoint          | blocking | get_zero_handler        |
        | SetZeroEndpoint          | blocking | set_zero_handler        |
        | AverageEndpoint          | spawn    | average_handler         |
    };
    topics_in: {
        list: TOPICS_IN_LIST;

        | TopicTy | kind | handler |
        | ------- | ---- | ------- |
    };
    topics_out: {
        list: TOPICS_OUT_LIST;
    };
}

fn interface_hash_handler(_context: &mut Context, _header: VarHeader, _rqst: ()) -> u64 {
    INTERFACE_HASH
}

fn info_handler(context: &mut Context, _header: VarHeader, _rqst: ()) -> DeviceInfo {
    DeviceInfo {
        unique_id: context.unique_id,
        sample_period_us: SAMPLE_PERIOD.as_micros() as u32,
    }
}

fn adc_status_handler(_context: &mut Context, _header: VarHeader, _rqst: ()) -> AdcStatus {
    ADC_STATUS.lock(|s| s.get())
}

fn time_handler(_context: &mut Context, _header: VarHeader, _rqst: ()) -> u64 {
    // Same clock as the readings' timestamps (`run_adcs` uses `Instant::now()`).
    Instant::now().as_micros()
}

fn get_calibration_handler(
    _context: &mut Context,
    _header: VarHeader,
    _rqst: (),
) -> CalibrationSetting {
    let state = CALIBRATION.lock(|s| s.get());
    CalibrationSetting {
        matrix: state.matrix,
        is_boot_default: state.matrix_is_boot_default,
    }
}

fn set_calibration_handler(
    _context: &mut Context,
    _header: VarHeader,
    matrix: CalibrationMatrix,
) -> SetResult {
    if !all_finite(matrix.as_flattened()) {
        return Err(SetError::NotFinite);
    }
    CALIBRATION.lock(|s| {
        s.set(CalibrationState {
            matrix,
            matrix_is_boot_default: false,
            ..s.get()
        })
    });
    defmt::info!("calibration matrix set");
    Ok(())
}

fn get_zero_handler(_context: &mut Context, _header: VarHeader, _rqst: ()) -> ZeroSetting {
    let state = CALIBRATION.lock(|s| s.get());
    ZeroSetting {
        counts: state.zero,
        is_boot_default: state.zero_is_boot_default,
    }
}

fn set_zero_handler(_context: &mut Context, _header: VarHeader, zero: ZeroCounts) -> SetResult {
    if !all_finite(&zero) {
        return Err(SetError::NotFinite);
    }
    CALIBRATION.lock(|s| {
        s.set(CalibrationState {
            zero,
            zero_is_boot_default: false,
            ..s.get()
        })
    });
    defmt::info!("zero offsets set: {}", zero);
    Ok(())
}

/// Runs as its own task (postcard-rpc `spawn` handler), so the 1 s wait doesn't
/// hold up other requests. Embassy tasks have one instance by default, so a
/// second `average` while this runs fails to spawn (`WireError::FailedToSpawn`).
#[embassy_executor::task]
async fn average_handler(_context: (), header: VarHeader, _rqst: (), sender: Sender<AppTx>) {
    // Clear a result left over from a run that timed out just as it completed.
    AVERAGE_DONE.reset();
    AVERAGING.lock(|a| *a.borrow_mut() = Some(CountsAccumulator::new()));

    let result: AverageResult = match with_timeout(AVERAGE_TIMEOUT, AVERAGE_DONE.wait()).await {
        Ok(Ok(accumulator)) => Ok(accumulator.finish()),
        Ok(Err(e)) => Err(e),
        Err(_timeout) => {
            AVERAGING.lock(|a| a.borrow_mut().take());
            Err(AverageError::Timeout)
        }
    };
    match &result {
        Ok(average) => defmt::info!("average: {}", average),
        Err(e) => defmt::warn!("average failed: {}", e),
    }
    if sender
        .reply::<AverageEndpoint>(header.seq_no, &result)
        .await
        .is_err()
    {
        defmt::warn!("average reply failed");
    }
}

/// Ends a running `average` with an error (no-op if none is running).
fn abort_average(error: AverageError) {
    if AVERAGING.lock(|a| a.borrow_mut().take()).is_some() {
        AVERAGE_DONE.signal(Err(error));
    }
}

/// Records a new status (`Terminated` is final) and wakes the publisher if it changed.
fn set_status(new: AdcStatus) {
    let changed = ADC_STATUS.lock(|s| {
        let old = s.get();
        if old == AdcStatus::Terminated || old == new {
            return false;
        }
        s.set(new);
        true
    });
    if changed {
        STATUS_CHANGED.signal(new);
    }
}

// ---------------------------------------------------------------------------
// Tasks
// ---------------------------------------------------------------------------

/// Drains the ADC stream as fast as it is filled and queues messages for the
/// publisher without ever waiting on it.
#[embassy_executor::task]
pub async fn pump(stream: Receiver<'static, CriticalSectionRawMutex, AdcsEvent, STREAM_DEPTH>) {
    loop {
        match stream.receive().await {
            AdcsEvent::Readings(timestamp, raw) => {
                let timestamp_us = timestamp.as_micros();

                // Measured 2026-11-24: mean 43.5 µs, max 47 µs per reading for the
                // copy + computation (about 1.1 % CPU at 250 Hz; software float).
                let state = CALIBRATION.lock(|s| s.get());
                let values = wrench_from_counts(&state.matrix, &state.zero, &raw);

                // Feed a running `average`; hand it over when complete.
                let complete = AVERAGING.lock(|a| {
                    let mut a = a.borrow_mut();
                    let accumulator = a.as_mut()?;
                    accumulator.push(&raw);
                    if accumulator.n() >= AVERAGE_READINGS {
                        a.take()
                    } else {
                        None
                    }
                });
                if let Some(accumulator) = complete {
                    AVERAGE_DONE.signal(Ok(accumulator));
                }

                // Dropped if full; gaps are visible in the timestamps.
                let _ = OUTGOING.try_send(Readings { timestamp_us, raw });
                let _ = WRENCHES.try_send(Wrench {
                    timestamp_us,
                    values,
                });
            }
            AdcsEvent::Error(e) => {
                defmt::warn!("ADC error: {}", e);
                set_status(AdcStatus::Errored);
                abort_average(AverageError::AdcError);
                // Dropped if full (see ERRORS); the status still says `Errored`.
                let _ = ERRORS.try_send(e);
            }
            AdcsEvent::Terminate => {
                defmt::error!("ADC sampling terminated");
                set_status(AdcStatus::Terminated);
                abort_average(AverageError::AdcError);
            }
        }
    }
}

/// Sends queued messages to the host. Blocks (inside the USB driver) while nobody
/// reads the serial port; the pump keeps dropping new messages meanwhile.
#[embassy_executor::task]
pub async fn publisher(sender: Sender<AppTx>) {
    loop {
        // `select4` polls in argument order, so status and errors go before readings
        // and wrenches when several are ready. The losing futures are dropped, which is
        // safe here: a dropped `receive()`/`wait()` doesn't consume anything.
        let result = match select4(
            STATUS_CHANGED.wait(),
            ERRORS.receive(),
            OUTGOING.receive(),
            WRENCHES.receive(),
        )
        .await
        {
            // Topic messages carry header sequence number 0: nothing uses it.
            Either4::First(status) => sender.publish::<AdcStatusTopic>(0u32.into(), &status).await,
            Either4::Second(error) => {
                let mut text = heapless::String::<MAX_LOG_LEN>::new();
                // On overflow, `write!` stops at the first piece that doesn't fit;
                // we send what was written up to there.
                let _ = write!(text, "ADC error: {error:?}");
                sender.log_str(&text).await
            }
            Either4::Third(readings) => {
                sender
                    .publish::<ReadingsTopic>(0u32.into(), &readings)
                    .await
            }
            Either4::Fourth(wrench) => sender.publish::<WrenchTopic>(0u32.into(), &wrench).await,
        };
        if result.is_err() {
            defmt::warn!("publish failed");
        }
    }
}
