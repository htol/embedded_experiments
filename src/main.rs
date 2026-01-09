#![no_std]
#![no_main]
#![allow(unused_imports, dead_code, unused_variables)]

mod pump;

use self::pump::PumpController;
use core::fmt::Write;
use core::ptr;
use core::sync::atomic::{AtomicU32, Ordering};
use defmt;
use embassy_executor::Spawner;
use embassy_stm32::exti::ExtiInput;
use embassy_stm32::gpio::{AnyPin, Input, Level, Output, OutputType, Pin, Pull, Speed};
use embassy_stm32::i2c::{I2c, Master};
use embassy_stm32::mode::Async;
use embassy_stm32::rcc::{self, Hse, HseMode, Pll, PllSource};
use embassy_stm32::time::{hz, khz, Hertz};
use embassy_stm32::timer::low_level::CountingMode;
use embassy_stm32::timer::qei::{Qei, QeiPin};
use embassy_stm32::timer::simple_pwm::{PwmPin, SimplePwm};
use embassy_stm32::timer::{CaptureCompareInterruptHandler, Channel};
use embassy_stm32::{bind_interrupts, pac, peripherals, Config};
use embassy_time::{Duration, Timer};
use embedded_hal::digital::{OutputPin, StatefulOutputPin};
use heapless::format;
use heapless::String;
use semihosting;

use ssd1306::mode::{TerminalDisplaySizeAsync, TerminalModeAsync};
use ssd1306::{prelude::*, I2CDisplayInterface, Ssd1306Async};
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

bind_interrupts!(struct Irqs {
    I2C2_EV => embassy_stm32::i2c::EventInterruptHandler<peripherals::I2C2>;
    I2C2_ER => embassy_stm32::i2c::ErrorInterruptHandler<peripherals::I2C2>;
    TIM3 => embassy_stm32::timer::CaptureCompareInterruptHandler<peripherals::TIM3>;
});

pub fn exit() -> ! {
    semihosting::process::exit(0);
}

static RPM: AtomicU32 = AtomicU32::new(0);
static LAST_PULSE_MS: AtomicU32 = AtomicU32::new(0);

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let mut config = Config::default();
    config.rcc.hse = None;
    config.rcc.pll = None;
    // Clock mismatch: Software thinks 72MHz, Hardware is 8MHz (HSI).
    // Factor = 9.

    let p = embassy_stm32::init(config);
    defmt::info!("System Init - Clock Mismatch Handled via Scaling");

    let led = Output::new(p.PC13, Level::High, Speed::Low);
    spawner.spawn(led_task(led)).unwrap();
    // encoder
    // --- QEI TIM1 (PA8, PA9) ---
    let encoder = Qei::new(p.TIM1, QeiPin::new(p.PA8), QeiPin::new(p.PA9));

    // Pump PWM
    // --- PWM on TIM2_CH1 (PA0) ---
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

    // spawn tachometer task
    spawner.spawn(tach_task(p.TIM3, p.PA6, p.EXTI6)).unwrap();

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

    // display
    let interface = I2CDisplayInterface::new(i2c);
    let mut display = Ssd1306Async::new(interface, DisplaySize128x64, DisplayRotation::Rotate0)
        .into_terminal_mode();
    display.init().await.unwrap();
    let _ = display.clear().await;

    loop {
        pump.update();

        let now = embassy_time::Instant::now().as_millis() as u32;
        let last_pulse = LAST_PULSE_MS.load(Ordering::Relaxed);

        let current_rpm = if now.wrapping_sub(last_pulse) > 1000 {
            0
        } else {
            RPM.load(Ordering::Relaxed)
        };

        let pct = if pump.min_duty() > 0 {
            pump.duty() * 20 / pump.min_duty()
        } else {
            0
        };

        let mut buf: heapless::String<LINE_LEN> = heapless::String::new();
        let _ = write!(buf, "rpm: {} d: {}%", current_rpm, pct);
        display_line(&mut display, 0, 0, &buf).await;

        // Clear line 2
        let mut empty: heapless::String<LINE_LEN> = heapless::String::new();
        for _ in 0..LINE_LEN {
            let _ = empty.push(' ');
        }
        display_line(&mut display, 0, 1, &empty).await;

        // Small delay to prevent display/I2C from hogging the bus
        Timer::after_millis(50).await;
    }
}

use embassy_stm32::Peri;

#[embassy_executor::task]
async fn tach_task(
    _tim: Peri<'static, peripherals::TIM3>,
    pin: Peri<'static, peripherals::PA6>,
    exti: Peri<'static, peripherals::EXTI6>,
) {
    // Hybrid Approach: EXTI Wait + TIM3 Hardware Latch (Raw Ptr)
    let mut tach = ExtiInput::new(pin, exti, Pull::Up);

    unsafe {
        // Enable TIM3 Clock (APB1) (Bit 1)
        pac::RCC.apb1enr().modify(|w| w.set_tim3en(true));

        // Reset TIM3
        pac::RCC.apb1rstr().modify(|w| w.set_tim3rst(true));
        pac::RCC.apb1rstr().modify(|w| w.set_tim3rst(false)); // Clear reset

        // Base Address of TIM3 on STM32F1 is 0x4000_0400.
        let base = 0x4000_0400 as *mut u32;

        // PSC (Offset 0x28). 10th u32.
        // 8MHz -> 1MHz (7)
        ptr::write_volatile(base.add(10), 7);

        // ARR (Offset 0x2C). 11th u32.
        ptr::write_volatile(base.add(11), 0xFFFF);

        // CCMR1 (Offset 0x18). 6th u32.
        // IC1F=0xF (Bits 4-7), CC1S=01 (Bits 0-1).
        // 0xF1.
        // Note: We overwrite entire register safely as we only use CH1.
        ptr::write_volatile(base.add(6), 0xF1);

        // CCER (Offset 0x20). 8th u32.
        // CC1E=1 (Bit 0).
        ptr::write_volatile(base.add(8), 1);

        // CR1 (Offset 0x00). 0th u32.
        // CEN=1 (Bit 0).
        ptr::write_volatile(base.add(0), 1);
    }

    let mut last_capture: u32 = 0;
    let mut count: u32 = 0;
    defmt::info!("Tach task started (Hybrid Raw TIM3)");

    loop {
        tach.wait_for_rising_edge().await;

        // Read CCR1 (Offset 0x34). 13th u32.
        let current_capture = unsafe { ptr::read_volatile((0x4000_0400 as *mut u32).add(13)) };

        let diff = if current_capture >= last_capture {
            current_capture - last_capture
        } else {
            (0xFFFF - last_capture) + current_capture + 1
        };

        last_capture = current_capture;
        let micros = diff;

        if micros > 100 {
            let rpm = 60_000_000 / (micros * 4);
            RPM.store(rpm, Ordering::Relaxed);
            LAST_PULSE_MS.store(
                embassy_time::Instant::now().as_millis() as u32,
                Ordering::Relaxed,
            );
            // if count % 20 == 0 {
            //     defmt::info!("micros: {}, rpm: {}", micros, rpm);
            // }
            count += 1;
        }
    }
}

#[embassy_executor::task]
async fn led_task(mut led: Output<'static>) {
    loop {
        let _ = led.toggle();
        Timer::after_millis(300).await; // set blink period
    }
}

const LINE_LEN: usize = 16; // line width in characters

// Function to format a fixed-length string
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

// Function to write a line to the display
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
