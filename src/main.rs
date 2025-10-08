#![no_std]
#![no_main]
#![allow(unused_imports, dead_code, unused_variables)]

mod pump;

use self::pump::PumpController;
use core::fmt::Write;
use defmt;
use embassy_executor::Spawner;
use embassy_stm32::gpio::{Level, Output, OutputType, Pull, Speed};
use embassy_stm32::i2c::{I2c, Master};
use embassy_stm32::mode::Async;
use embassy_stm32::rcc::{self, Hse, HseMode, Pll};
use embassy_stm32::time::{hz, khz};
use embassy_stm32::timer::input_capture::{CapturePin, InputCapture};
use embassy_stm32::timer::low_level::CountingMode;
use embassy_stm32::timer::qei::{Qei, QeiPin};
use embassy_stm32::timer::simple_pwm::{PwmPin, SimplePwm};
use embassy_stm32::timer::{CaptureCompareInterruptHandler, Channel};
use embassy_stm32::{bind_interrupts, peripherals, Config};
use embassy_time::{Duration, Timer};
use embedded_hal::digital::{OutputPin, StatefulOutputPin};
use heapless::format;
use heapless::String;
use semihosting;

use ssd1306::mode::{TerminalDisplaySizeAsync, TerminalModeAsync};
use ssd1306::{prelude::*, I2CDisplayInterface, Ssd1306Async};
use {defmt_rtt as _, panic_probe as _};

bind_interrupts!(struct Irqs {
    TIM3 => CaptureCompareInterruptHandler<peripherals::TIM3>;
    I2C2_EV => embassy_stm32::i2c::EventInterruptHandler<peripherals::I2C2>;
    I2C2_ER => embassy_stm32::i2c::ErrorInterruptHandler<peripherals::I2C2>;
});

pub fn exit() -> ! {
    semihosting::process::exit(0);
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let mut config = Config::default();
    config.rcc.hse = Some(Hse {
        freq: embassy_stm32::time::Hertz(8_000_000),
        mode: HseMode::Oscillator,
    });

    config.rcc.pll = Some(Pll {
        src: embassy_stm32::rcc::PllSource::HSE,
        prediv: embassy_stm32::rcc::PllPreDiv::DIV1,
        mul: embassy_stm32::rcc::PllMul::MUL9,
    });

    let p = embassy_stm32::init(config);

    let led = Output::new(p.PC13, Level::High, Speed::Low);
    spawner.spawn(led_task(led)).unwrap();
    // encoder
    // --- QEI TIM1 (PA8, PA9) ---
    let encoder = Qei::new(p.TIM1, QeiPin::new(p.PA8), QeiPin::new(p.PA9));

    // Pump PWM
    // --- PWM на TIM2_CH1 (PA0) ---
    let mut pwm = SimplePwm::new(
        p.TIM2,
        Some(PwmPin::new(p.PA0, OutputType::PushPull)),
        None,
        None,
        None,
        embassy_stm32::time::hz(25_000),
        embassy_stm32::timer::low_level::CountingMode::EdgeAlignedUp,
    );
    pwm.ch1().enable();
    let mut pump = PumpController::new(pwm, encoder);

    // pump tachometer
    let tach_timer_freq = 1_000_000;
    let mut tach = InputCapture::new(
        p.TIM3,
        Some(CapturePin::new(p.PA6, Pull::Up)),
        None,
        None,
        None,
        Irqs,
        hz(tach_timer_freq),
        CountingMode::EdgeAlignedUp,
    );
    tach.set_input_capture_mode(
        Channel::Ch1,
        embassy_stm32::timer::low_level::InputCaptureMode::Rising,
    );
    tach.enable(Channel::Ch1);
    let mut last_capture: Option<u32> = None;
    let pulses_per_rev = 1;
    defmt::info!("{}", rcc::clocks(&p.RCC));
    defmt::info!("tach enabled {}", tach.is_enabled(Channel::Ch1));

    // i2c
    let i2c = embassy_stm32::i2c::I2c::new(
        p.I2C2,
        p.PB10,
        p.PB11,
        Irqs,
        p.DMA1_CH4,
        p.DMA1_CH5,
        Default::default(),
    );

    // dsplay
    let interface = I2CDisplayInterface::new(i2c);
    let mut display = Ssd1306Async::new(interface, DisplaySize128x64, DisplayRotation::Rotate0)
        .into_terminal_mode();
    display.init().await.unwrap();
    let _ = display.clear().await;

    let mut buff: String<32> = String::new();

    const TIM_MAX: u32 = 0xFFFF;
    loop {
        buff.clear();
        pump.update();
        tach.wait_for_rising_edge(Channel::Ch1).await;
        let tach_value = tach.get_capture_value(Channel::Ch1);
        if let Some(prev) = last_capture {
            let ticks = if tach_value >= prev {
                tach_value - prev
            } else {
                (TIM_MAX + 1 - prev) + tach_value
            };
            let period = ticks as f32 / tach_timer_freq as f32; // getting period in seconds
            let freq = 1.0 / period;
            let rpm = freq as f32 * 60.0 / pulses_per_rev as f32;
            let rpm_u = rpm as u32;
            defmt::info!(
                "capture: {}, ticks: {}, period: {}, freq:{}, rpm: {}, rpm_u: {}",
                tach_value,
                ticks,
                period,
                freq,
                rpm,
                rpm_u
            );
            if let Err(e) = write!(&mut buff, "capture: {}\nrpm: {}", tach_value, rpm_u) {
                defmt::warn!("Failed to write to buffer");
            }
            //let _ = display.clear().await;
            let line1 = make_line("cap:", tach_value);
            let line2 = make_line("rpm:", rpm as u32);
            //display_line(&mut display, 0, 0, &line1).await;
            display_line(&mut display, 0, 1, &line2).await;
        }
        last_capture = Some(tach_value);
    }
}

#[embassy_executor::task]
async fn led_task(mut led: Output<'static>) {
    loop {
        let _ = led.toggle();
        Timer::after_millis(300).await; // задаём период мигания
    }
}

const LINE_LEN: usize = 16; // ширина строки в символах

// Функция для формирования строки фиксированной длины
fn make_line(prefix: &str, s: impl core::fmt::Display) -> heapless::String<LINE_LEN> {
    let mut buf: heapless::String<LINE_LEN> = heapless::String::new();
    let _ = write!(
        buf,
        "{}{:>width$}",
        prefix,
        s,
        width = LINE_LEN - prefix.len()
    );
    buf
}

// Функция для записи двух строк на дисплей
async fn display_line(
    display: &mut Ssd1306Async<
        I2CInterface<I2c<'_, Async, Master>>,
        DisplaySize128x64,
        TerminalModeAsync,
    >,
    col: u8,
    row: u8,
    line: &heapless::String<LINE_LEN>,
) {
    if let Err(_) = display.set_position(col, row).await {
        defmt::warn!("Failed to set cursor for line 1");
    }
    if let Err(_) = display.write_str(line).await {
        defmt::warn!("Failed to write line to c: {}, r: {}:", col, row);
    }
}
