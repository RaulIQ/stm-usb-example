#![no_std]
#![no_main]

use defmt::{panic, *};
use embassy_executor::{task, Spawner};
use embassy_futures::join::join;
use embassy_stm32::gpio::{Level, Output, Speed};
use embassy_stm32::time::Hertz;
use embassy_stm32::usb::{Driver, Instance};
use embassy_stm32::{bind_interrupts, peripherals, usb, Config};
use embassy_time::{Duration, Timer};
use embassy_usb::class::cdc_acm::{CdcAcmClass, State};
use embassy_usb::driver::EndpointError;
use embassy_usb::Builder;
use postcard::{from_bytes, to_vec};
use serde::{Deserialize, Serialize};
use {defmt_rtt as _, panic_probe as _};

bind_interrupts!(struct Irqs {
    OTG_FS => usb::InterruptHandler<peripherals::USB_OTG_FS>;
});

// #[task]
// async fn blink(mut led: Output<'static>) {
//     loop {
//         led.set_high();
//         Timer::after(Duration::from_millis(800)).await;
//         led.set_low();
//         Timer::after(Duration::from_millis(800)).await;
//     }
// }


#[derive(Serialize, Deserialize)]
enum E {
    SomeError
}

#[derive(Serialize, Deserialize)]
enum Request {
    StartSending,
    GetMessage,
    SendingCompleted
}

#[derive(Serialize, Deserialize)]
enum Response {
    SendingStarted,
    Message(Option<LogMessage>),
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Copy, Clone)]
pub enum LogMessage {
    Accel {
        x: f32,
        y: f32,
        z: f32,
    },
    Gyro {
        x: f32,
        y: f32,
        z: f32,
    },
    Mag {
        x: f32,
        y: f32,
        z: f32,
    },
    Motors {
        m1: f32,
        m2: f32,
        m3: f32,
        m4: f32,
    },
    Batt {
        v: f32,
        a: f32,
    },
}



#[embassy_executor::main]
async fn main(spawner: Spawner) {

    let mut config = embassy_stm32::Config::default();

    {
        use embassy_stm32::rcc::*;
        config.rcc.hse = Some(Hse {
            freq: Hertz(8_000_000),
            mode: HseMode::Bypass,
        });
        config.rcc.pll_src = PllSource::HSE;
        config.rcc.pll = Some(Pll {
            prediv: PllPreDiv::DIV4,
            mul: PllMul::MUL216,
            divp: Some(PllPDiv::DIV2), // 8mhz / 4 * 216 / 2 = 216Mhz
            divq: Some(PllQDiv::DIV9), // 8mhz / 4 * 216 / 9 = 48Mhz
            divr: None,
        });
        config.rcc.ahb_pre = AHBPrescaler::DIV1;
        config.rcc.apb1_pre = APBPrescaler::DIV4;
        config.rcc.apb2_pre = APBPrescaler::DIV2;
        config.rcc.sys = Sysclk::PLL1_P;
        config.rcc.mux.clk48sel = mux::Clk48sel::PLL1_Q;
    }

    let mut p = embassy_stm32::init(config);
    println!("hello");

    // spawner.spawn(blink(blue)).unwrap();

    // Create the driver, from the HAL.
    let mut ep_out_buffer = [0u8; 256];
    let mut config = embassy_stm32::usb::Config::default();

    // Do not enable vbus_detection. This is a safe default that works in all boards.
    // However, if your USB device is self-powered (can stay powered on if USB is unplugged), you need
    // to enable vbus_detection to comply with the USB spec. If you enable it, the board
    // has to support it or USB won't work at all. See docs on `vbus_detection` for details.
    config.vbus_detection = true;

    let driver = Driver::new_fs(p.USB_OTG_FS, Irqs, p.PA12, p.PA11, &mut ep_out_buffer, config);

    // Create embassy-usb Config
    let mut config = embassy_usb::Config::new(0xc0de, 0xcafe);
    config.manufacturer = Some("Embassy");
    config.product = Some("USB-serial example");
    config.serial_number = Some("12345678");

    // Create embassy-usb DeviceBuilder using the driver and config.
    // It needs some buffers for building the descriptors.
    let mut config_descriptor = [0; 256];
    let mut bos_descriptor = [0; 256];
    let mut control_buf = [0; 64];

    let mut state = State::new();

    let mut builder = Builder::new(
        driver,
        config,
        &mut config_descriptor,
        &mut bos_descriptor,
        &mut [], // no msos descriptors
        &mut control_buf,
    );

    // Create classes on the builder.
    let mut class = CdcAcmClass::new(&mut builder, &mut state, 64);

    // Build the builder.
    let mut usb = builder.build();

    // Run the USB device.
    let usb_fut = usb.run();

    let mut messages = [
        LogMessage::Accel { x: 123.3, y: 123.4, z: 123.68 },
        LogMessage::Gyro { x: 13.123, y: 15.341, z: 56.543 },
        LogMessage::Batt { v: 16.4, a: 52.6 },
        LogMessage::Batt { v: 16.4, a: 52.6 },
        LogMessage::Gyro { x: 13.123, y: 15.341, z: 56.543 },
        LogMessage::Accel { x: 123.3, y: 123.4, z: 123.68 },
    ];

    // Do stuff with the class!
    let echo_fut = async {
        loop {
            class.wait_connection().await;
            info!("Connected");
            let _ = send_data(&mut class, &mut messages).await;
            info!("Disconnected");
        }
    };

    // Run everything concurrently.
    // If we had made everything `'static` above instead, we could do this using separate tasks instead.
    join(usb_fut, echo_fut).await;
}

struct Disconnected {}

impl From<EndpointError> for Disconnected {
    fn from(val: EndpointError) -> Self {
        match val {
            EndpointError::BufferOverflow => panic!("Buffer overflow"),
            EndpointError::Disabled => Disconnected {},
        }
    }
}

async fn echo<'d, T: Instance + 'd>(class: &mut CdcAcmClass<'d, Driver<'d, T>>) -> Result<(), Disconnected> {
    let mut buf = [0; 64];
    let hello = "hello, ".as_bytes();

    let len = core::cmp::min(hello.len(), buf.len());
    buf[..len].copy_from_slice(&hello[..len]);
    info!("{:?}", buf);
    loop {
        let n = class.read_packet(&mut buf[len..]).await?;
        let data = &buf[..(n + len)];
        info!("data: {:x}", data);
        class.write_packet(data).await?;
        
    }
}

async fn send_data<'d, T: Instance + 'd>(
    class: &mut CdcAcmClass<'d, Driver<'d, T>>,
    messages: &[LogMessage]
) -> Result<(), Disconnected>{
    let mut buf = [0; 64];
    loop {
        let n = class.read_packet(&mut buf).await?;
        let data = &buf[..n];
        let command: Request = from_bytes(&data).unwrap();
        match command {
            Request::StartSending => {
                println!("start");

                let data = to_vec::<Response, 32>(&Response::SendingStarted).unwrap();
                class.write_packet(&data).await?;

                for message in messages.iter() {
                    let response = Response::Message(Some(message.clone()));
                    let data = to_vec::<Response, 32>(&response).unwrap();
                    class.write_packet(&data).await?;
                    let n = class.read_packet(&mut buf).await?;
                    let data = &buf[..n];
                    let command: Request = from_bytes(&data).unwrap();
                    match command {
                        Request::GetMessage => continue,
                        _ => ()
                    }
                }
                let data = to_vec::<Response, 32>(&Response::Message(None)).unwrap();
                class.write_packet(&data).await?;
            },
            _ => ()
        }
    }
}