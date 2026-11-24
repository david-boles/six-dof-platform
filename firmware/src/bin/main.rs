#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use embassy_sync::channel::{Channel, SendDynamicSender};
use esp_hal::clock::CpuClock;
use esp_hal::efuse;
use esp_hal::timer::systimer::SystemTimer;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::usb::usb_serial_jtag::UsbSerialJtag;

use embassy_executor::Spawner;

use defmt::{info, warn};

use panic_rtt_target as _;
use postcard_rpc::server::{Dispatch, Server};
use six_dof_platform::comms::{self, App, AppServer, Context, RX_FRAME_LEN, STORAGE, Stream};
use six_dof_platform::input_adcs::run_adcs;
use static_cell::{ConstStaticCell, StaticCell};

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

static STREAM: StaticCell<Stream> = StaticCell::new();
static PACKET_RX_BUF: ConstStaticCell<[u8; RX_FRAME_LEN]> =
    ConstStaticCell::new([0u8; RX_FRAME_LEN]);

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

    // postcard-rpc server over the USB-Serial-JTAG serial port (CDC-ACM).
    let (usb_rx, usb_tx) = UsbSerialJtag::new(peripherals.USB_DEVICE)
        .into_async()
        .split();
    let (rx_impl, tx_impl) = STORAGE
        .init(usb_rx, usb_tx)
        .expect("STORAGE initialized twice");
    let context = Context {
        unique_id: unique_id(),
    };
    let dispatcher = App::new(context, spawner.into());
    let key_kind = dispatcher.min_key_len();
    let mut server: AppServer =
        Server::new(tx_impl, rx_impl, PACKET_RX_BUF.take(), dispatcher, key_kind);

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
    spawner.spawn(comms::pump(stream.receiver()).expect("pump spawn failed"));
    spawner.spawn(comms::publisher(server.sender()).expect("publisher spawn failed"));

    loop {
        // Only returns on a wire error; just keep serving.
        let e = server.run().await;
        warn!("postcard-rpc server: {}", e);
    }
}

/// Derived from the chip's factory MAC address.
fn unique_id() -> u64 {
    let mac = efuse::base_mac_address();
    let mac = mac.as_bytes();
    u64::from_le_bytes([mac[0], mac[1], mac[2], mac[3], mac[4], mac[5], 0, 0])
}
