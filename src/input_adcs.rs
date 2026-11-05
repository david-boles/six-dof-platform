use core::{cell::RefCell, ops::Not};

use critical_section::Mutex;
use defmt::{Format, info};
use embassy_sync::{
    blocking_mutex::raw::CriticalSectionRawMutex, channel::SendDynamicSender, signal::Signal,
};
use embassy_time::{Duration, Instant, Timer};
use esp_hal::{
    Blocking, handler,
    i2c::master::{Config as I2cConfig, Error, I2c},
    peripherals::{GPIO22, GPIO23, I2C0},
    time::{Duration as HalDuration, Rate},
    timer::{AnyTimer, PeriodicTimer},
};

use crate::ads122c04::{
    self, Config, Config0, Config1, Config2, Config3, DataRate, Gain, IdacMux, Mux, Register, Vref,
    read_data, reset, start, validate_inversion,
};

const ACCEPTABLE_ERROR_PERIOD: Duration = Duration::from_secs(1);

/// Period of the ISR. The ISR's I2C work (3 x read + mux write + start)
/// plus the actual ADC measurement time must both fit inside it.
const HALF_SAMPLE_PERIOD: HalDuration = HalDuration::from_millis(2);

const ADC_ADDRS: [u8; 3] = [0x40, 0x41, 0x44];

const CONFIG0: Config0 = Config0::new()
    .with_mux(Mux::Ain0Ain1)
    .with_gain(Gain::G1)
    .with_pga_bypass(false);

const CONFIG: Config = Config::new()
    .with_config0(CONFIG0)
    .with_config1(
        Config1::new()
            .with_data_rate(DataRate::Normal600Turbo1200)
            .with_turbo_mode(true)
            .with_continuous_mode(false)
            .with_vref(Vref::External)
            .with_temperature_mode(false),
    )
    .with_config2(
        Config2::new()
            .with_data_counter_en(true)
            .with_data_integrity(ads122c04::DataIntegrity::Inverted)
            .with_burn_out_current_sources_en(false)
            .with_idac_current(ads122c04::IdacCurrent::Off),
    )
    .with_config3(
        Config3::new()
            .with_idac1_mux(IdacMux::Disabled)
            .with_idac2_mux(IdacMux::Disabled),
    );

/// Errors include which ADC's I2C address they resulted from.
#[derive(Format, Debug)]
pub enum AdcsError {
    I2c(u8, &'static str, Error),
    Counter {
        addr: u8,
        expected: u8,
        received: u8,
    },
    DataIntegrity(u8),
    StreamFull,
}

#[derive(Format, Debug)]
pub enum AdcsEvent {
    Readings(Instant, [i32; 6]),
    /// Errors are not fatal. Task will try to re-initialize.
    Error(AdcsError),
    /// If errors exceed acceptable threshold, the task will terminate.
    Terminate,
}

/// The resources used by the task and ISR. Ownership is of these resources back and forth between them.
struct Resources {
    stream: SendDynamicSender<'static, AdcsEvent>,
    i2c: I2c<'static, Blocking>,
    timer: PeriodicTimer<'static, Blocking>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AdcChannel {
    First,
    Second,
}

impl AdcChannel {
    const fn config0(&self) -> Config0 {
        CONFIG0.with_mux(match self {
            AdcChannel::First => Mux::Ain0Ain1,
            AdcChannel::Second => Mux::Ain2Ain3,
        })
    }

    fn meas_idx(&self, adc_idx: usize) -> usize {
        (2 * adc_idx)
            + match self {
                AdcChannel::First => 0,
                AdcChannel::Second => 1,
            }
    }
}

impl Not for AdcChannel {
    type Output = AdcChannel;
    fn not(self) -> Self::Output {
        match self {
            AdcChannel::First => AdcChannel::Second,
            AdcChannel::Second => AdcChannel::First,
        }
    }
}

/// Everything the ISR owns while sampling.
struct IsrState {
    resources: Resources,
    measurement_underway: Option<AdcChannel>,
    expected_counter: u8,
    measurements: [i32; 6],
}

impl IsrState {
    fn new(resources: Resources) -> Self {
        IsrState {
            resources,
            measurement_underway: None,
            expected_counter: 1,
            measurements: [0; 6],
        }
    }
}

/// Handoff of the hardware between the task and the ISR.
static ISR_STATE: Mutex<RefCell<Option<IsrState>>> = Mutex::new(RefCell::new(None));
/// The ISR signals this when it hits an error and stops itself.
static ISR_ERROR: Signal<CriticalSectionRawMutex, AdcsError> = Signal::new();

// XXX: May want to evaluate if a max priority interrupt
// and blocking other interrupts for the duration is a good idea.
// For now though I want maximally consistent timing.
#[handler(priority = esp_hal::interrupt::Priority::max())]
fn adcs_isr() {
    critical_section::with(|cs| {
        let mut state = ISR_STATE.borrow_ref_mut(cs);
        let state = state.as_mut().expect("adc_isr run without state");

        state.resources.timer.clear_interrupt();

        // Terminate sampling and have the task reinitialize the ADCs on error.
        if let Err(e) = adcs_isr_impl(state) {
            state.resources.timer.unlisten();
            let _ = state.resources.timer.cancel(); // XXX: Stops the timer, erroring if already stopped.
            ISR_ERROR.signal(e);
        }
    });
}

fn adcs_isr_impl(state: &mut IsrState) -> Result<(), AdcsError> {
    if let Some(measurement_underway) = state.measurement_underway {
        // Record measurements
        for (adc_idx, addr) in ADC_ADDRS.iter().enumerate() {
            let mut buf = [0; 8];
            read_data(&mut state.resources.i2c, *addr, &mut buf)
                .map_err(|e| AdcsError::I2c(*addr, "read", e))?;
            validate_inversion(&buf).map_err(|_| AdcsError::DataIntegrity(*addr))?;

            (buf[0] == state.expected_counter).ok_or(AdcsError::Counter {
                addr: *addr,
                expected: state.expected_counter,
                received: buf[0],
            })?;
            state.measurements[measurement_underway.meas_idx(adc_idx)] =
                i32::from_be_bytes([buf[1], buf[2], buf[3], 0]) >> 8;
        }

        // Both channels have been measured; send them!
        if measurement_underway == AdcChannel::Second {
            state
                .resources
                .stream
                .try_send(AdcsEvent::Readings(Instant::now(), state.measurements))
                .map_err(|_| AdcsError::StreamFull)?;
        }

        // Update state for validating the next results.
        state.expected_counter = state.expected_counter.wrapping_add(1);
    }

    // Initiate the measurements that will be read on the next ISR trigger
    let next_channel = state
        .measurement_underway
        .map(|ch| !ch)
        .unwrap_or(AdcChannel::First);

    for addr in ADC_ADDRS {
        next_channel
            .config0()
            .write(&mut state.resources.i2c, addr)
            .map_err(|e| AdcsError::I2c(addr, "mux", e))?;
        start(&mut state.resources.i2c, addr).map_err(|e| AdcsError::I2c(addr, "start", e))?;
    }

    state.measurement_underway = Some(next_channel);

    Ok(())
}

#[embassy_executor::task]
pub async fn run_adcs(
    stream: SendDynamicSender<'static, AdcsEvent>,
    i2c: I2C0<'static>,
    sda: GPIO22<'static>,
    scl: GPIO23<'static>,
    timer: AnyTimer<'static>,
) {
    // XXX: It seems like even with using a hardware timer and ISR,
    // sometimes we fail to meet timing right at the beginning
    // (I2C traffic stops for ~7ms after ~30ms).
    // I'm guessing this is a quirk of the debugger/execution setup?
    // Waiting a smidge seems to make it reliable.
    Timer::after(Duration::from_millis(50)).await;

    // Initialize resources
    let i2c = I2c::new(i2c, I2cConfig::default().with_frequency(Rate::from_mhz(1)))
        .expect("i2c setup")
        .with_sda(sda)
        .with_scl(scl);
    let mut timer = PeriodicTimer::new(timer);
    timer.set_interrupt_handler(adcs_isr);
    let mut resources = Resources { stream, i2c, timer };

    let mut last_error_time: Option<Instant> = None;

    loop {
        let (err, yielded_resources) = run_adcs_impl(resources).await;
        resources = yielded_resources;

        // Errors are only sent here, in an async context, so that we can wait for the stream to have space; they never get dropped.
        stream.send(AdcsEvent::Error(err)).await;

        if let Some(last_error_time) = last_error_time
            && last_error_time.elapsed() < ACCEPTABLE_ERROR_PERIOD
        {
            stream.send(AdcsEvent::Terminate).await;
            return;
        }

        last_error_time = Some(Instant::now());
    }
}

/// Initializes the ADCs, hands the hardware to the periodic ISR, and waits for it to fail.
/// Only returns on error, giving back the I2c instance so the task can attempt to re-initialize.
async fn run_adcs_impl(mut resources: Resources) -> (AdcsError, Resources) {
    // Reset and configure ADCs
    for addr in ADC_ADDRS {
        if let Err(e) = reset(&mut resources.i2c, addr) {
            return (AdcsError::I2c(addr, "reset", e), resources);
        }
    }
    // XXX: They do seem to need a little time after reset to accept (ACK) configuration, hence the separate loop.
    // The datasheet says they shouldn't if I2c timing isn't violated but I haven't investigated.
    for addr in ADC_ADDRS {
        if let Err(e) = CONFIG.write(&mut resources.i2c, addr) {
            return (AdcsError::I2c(addr, "config", e), resources);
        }
    }
    info!("adc init");

    // Begin periodically calling the ISR to start and send measurements.
    ISR_ERROR.reset();
    critical_section::with(|cs| {
        let mut state = ISR_STATE.borrow_ref_mut(cs);
        state.replace(IsrState::new(resources));
        let state = state.as_mut().expect("adc_isr run without state");
        state.resources.timer.listen();
        state
            .resources
            .timer
            .start(HALF_SAMPLE_PERIOD)
            .expect("only fallible depending on period");
    });

    // Wait indefinitely unless the ISR errors.
    (
        ISR_ERROR.wait().await,
        critical_section::with(|cs| {
            ISR_STATE
                .take(cs)
                .expect("isr errored, state should still be present")
                .resources
        }),
    )
}
