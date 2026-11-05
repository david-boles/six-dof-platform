use defmt::{Format, info};
use embassy_sync::channel::DynamicSender;
use embassy_time::{Delay, Duration, Instant, Ticker, Timer};
use esp_hal::{
    Blocking,
    i2c::master::{Config as I2cConfig, Error, I2c},
    peripherals::{GPIO22, GPIO23, I2C0},
    time::Rate,
};

use crate::ads122c04::{
    self, Config, Config0, Config1, Config2, Config3, DataRate, Gain, IdacMux, Mux, Register, Vref,
    read_data, reset, start, validate_inversion,
};

const ACCEPTABLE_ERROR_PERIOD: Duration = Duration::from_secs(1);
const ADC_ADDRS: [u8; 3] = [0x40, 0x41, 0x44];

/// Errors include which ADC's I2C address they resulted from.
#[derive(Format)]
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

#[derive(Format)]
pub enum AdcsEvent {
    Readings(Instant, [i32; 6]),
    /// Errors are not fatal. Task will try to re-initialize.
    Error(AdcsError),
    /// If errors exceed acceptable threshold, the task will terminate.
    Terminate,
}

#[embassy_executor::task]
pub async fn run_adcs(
    stream: DynamicSender<'static, AdcsEvent>,
    i2c: I2C0<'static>,
    sda: GPIO22<'static>,
    scl: GPIO23<'static>,
) {
    let mut i2c = I2c::new(i2c, I2cConfig::default().with_frequency(Rate::from_mhz(1)))
        .expect("i2c setup")
        .with_sda(sda)
        .with_scl(scl);

    let mut last_error_time: Option<Instant> = None;

    loop {
        let err = run_adcs_impl(&stream, &mut i2c)
            .await
            .err()
            .expect("impl should only return on error");
        stream.send(AdcsEvent::Error(err)).await;

        if let Some(last_error_time) = last_error_time {
            if last_error_time.elapsed() < ACCEPTABLE_ERROR_PERIOD {
                stream.send(AdcsEvent::Terminate).await;
                return;
            }
        }

        last_error_time = Some(Instant::now());
    }
}

async fn run_adcs_impl(
    stream: &DynamicSender<'static, AdcsEvent>,
    i2c: &mut I2c<'_, Blocking>,
) -> Result<(), (AdcsError)> {
    const CONFIG0: Config0 = Config0::new()
        .with_mux(Mux::Ain0Ain1)
        .with_gain(Gain::G1)
        .with_pga_bypass(false);

    const CONFIG: Config = Config::new()
        .with_config0(CONFIG0)
        .with_config1(
            Config1::new()
                .with_data_rate(DataRate::Normal1000Turbo2000)
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

    for addr in ADC_ADDRS {
        reset(i2c, addr).map_err(|e| AdcsError::I2c(addr, "reset", e))?;
    }
    for addr in ADC_ADDRS {
        CONFIG
            .write(i2c, addr)
            .map_err(|e| AdcsError::I2c(addr, "config", e))?;
    }

    info!("adc init");

    // For each ADC, read the last measurement and start a new measurement on the alternate channel every 1ms.
    // With turbo and speed settings they should take 0.5ms to take a measurement... even with timing jitter they should always be ready to be read.

    let mut measure_second_channel = false;
    let mut expected_counter: u8 = 1;
    let mut ticker = Ticker::every(Duration::from_millis(20));
    let mut measurements = [0i32; 6];

    loop {
        for addr in ADC_ADDRS {
            let config0 = CONFIG0.with_mux(match measure_second_channel {
                false => Mux::Ain0Ain1,
                true => Mux::Ain2Ain3,
            });
            config0
                .write(i2c, addr)
                .map_err(|e| AdcsError::I2c(addr, "mux", e))?;
            start(i2c, addr).map_err(|e| AdcsError::I2c(addr, "start", e))?;
        }

        ticker.next().await;

        for (adc_idx, addr) in ADC_ADDRS.iter().enumerate() {
            let meas_idx = (2 * adc_idx)
                + match measure_second_channel {
                    false => 0,
                    true => 1,
                };

            let mut buf = [0; 8];
            read_data(i2c, *addr, &mut buf).map_err(|e| AdcsError::I2c(*addr, "read", e))?;
            validate_inversion(&buf).map_err(|_| AdcsError::DataIntegrity(*addr))?;

            (buf[0] == expected_counter).ok_or(AdcsError::Counter {
                addr: *addr,
                expected: expected_counter,
                received: buf[0],
            })?;
            measurements[meas_idx] = i32::from_be_bytes([buf[1], buf[2], buf[3], 0]) >> 8;
        }

        // One complete set of measurements is complete; send it!
        if measure_second_channel {
            stream
                .try_send(AdcsEvent::Readings(Instant::now(), measurements))
                .map_err(|_| AdcsError::StreamFull)?;
        }

        measure_second_channel = !measure_second_channel;
        expected_counter = expected_counter.wrapping_add(1);
    }
}
