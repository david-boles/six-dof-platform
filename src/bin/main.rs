#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]


use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Channel, SendDynamicSender};
use esp_hal::timer::systimer::SystemTimer;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::clock::CpuClock;

use embassy_executor::Spawner;

use defmt::info;

use panic_rtt_target as _;
use six_dof_platform::input_adcs::{AdcsEvent, run_adcs};
use static_cell::StaticCell;

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

static STREAM: StaticCell<Channel<CriticalSectionRawMutex, AdcsEvent, 16>> = StaticCell::new();

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    // generator version: 1.4.0
    // generator parameters: -o esp32c6 -o unstable-hal -o embassy -o probe-rs -o defmt -o panic-rtt-target -o cargo-embed -o vscode

    rtt_target::rtt_init_defmt!();

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    info!("Embassy initialized!");

    let stream = STREAM.init(Channel::new());
    let systimer = SystemTimer::new(peripherals.SYSTIMER);
    spawner.spawn(
        run_adcs(
            SendDynamicSender::from(stream.sender()),
            peripherals.I2C0,
            peripherals.GPIO22,
            peripherals.GPIO23,
            systimer.alarm0.into(),
        )
        .expect("run_adcs spawn failed"),
    );

    loop {
        let event = stream.receive().await;
        // match event {
        //     AdcsEvent::Readings(_, [_, _, a, b, _, _]) => {
        //         let a = ((a + 2000) / 80) as usize;
        //         let b = ((b + 2000) / 80) as usize;
        //         const BAR: &'static str = "||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||";
        //         const SPACE: &'static str = "                                                                                                              ";
        //         info!(
        //             "{}{}\t{}{}",
        //             BAR[0..a],
        //             SPACE[0..(50 - a)],
        //             BAR[0..b],
        //             SPACE[0..(50 - b)]
        //         )
        //     }
        //     _ => info!("{}", event),
        // }
        match event {
            AdcsEvent::Readings(_, _) => {}
            _ => panic!("{:?}", event),
        }
        info!("{}", event);
    }
}
