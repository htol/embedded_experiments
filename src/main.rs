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
use embassy_stm32::adc::{Adc, SampleTime};
use embassy_stm32::exti::ExtiInput;
use embassy_stm32::gpio::{AnyPin, Input, Level, Output, OutputType, Pin, Pull, Speed};
use embassy_stm32::i2c::{I2c, Master};
use embassy_stm32::mode::Blocking;
use embassy_stm32::rcc::{self, Hse, HseMode, Pll, PllSource};
use embassy_stm32::time::{hz, khz, Hertz};
use embassy_stm32::timer::low_level::CountingMode;
use embassy_stm32::timer::qei::{Qei, QeiPin};
use embassy_stm32::timer::simple_pwm::{PwmPin, SimplePwm};
use embassy_stm32::timer::{CaptureCompareInterruptHandler, Channel};
use embassy_stm32::{bind_interrupts, pac, peripherals, Config};
use embassy_time::{with_timeout, Duration, Timer};
use embedded_hal::digital::{OutputPin, StatefulOutputPin};
use heapless::format;
use heapless::String;
use libm;
use semihosting;

use embedded_graphics::mono_font::ascii::FONT_10X20;
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::text::{Baseline, Text};
use ssd1306::mode::BufferedGraphicsMode;
use ssd1306::{prelude::*, I2CDisplayInterface, Ssd1306};

// ... existing imports ...
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

bind_interrupts!(struct Irqs {
    I2C2_EV => embassy_stm32::i2c::EventInterruptHandler<peripherals::I2C2>;
    I2C2_ER => embassy_stm32::i2c::ErrorInterruptHandler<peripherals::I2C2>;
    TIM3 => embassy_stm32::timer::CaptureCompareInterruptHandler<peripherals::TIM3>;
    ADC1_2 => embassy_stm32::adc::InterruptHandler<peripherals::ADC1>;
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

    let mut p = embassy_stm32::init(config);
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
    let mut i2c_config = embassy_stm32::i2c::Config::default();
    i2c_config.frequency = khz(100);
    i2c_config.timeout = embassy_time::Duration::from_millis(100); // Hardware timeout too
    let i2c = embassy_stm32::i2c::I2c::new_blocking(p.I2C2, p.PB10, p.PB11, i2c_config);

    // ADC init
    // ADC init - assume new takes 1 arg (delay might be internal or not needed?)
    let mut adc = Adc::new(p.ADC1);
    // adc.set_sample_time(SampleTime::Cycles71_5); // variant issue, using default

    // display
    let interface = I2CDisplayInterface::new(i2c);
    let mut display = Ssd1306::new(interface, DisplaySize128x64, DisplayRotation::Rotate0)
        .into_buffered_graphics_mode();
    display.init().unwrap();
    display.clear(BinaryColor::Off).unwrap();

    // --- Hybrid Calibration / Default Sequence ---
    // Button on PB12 (Input with PullUp)
    // - Low (Pressed): Run Calibration Sweep
    // - High (Released): Use Hardcoded Defaults (15% - 45%)

    let calib_button = Input::new(p.PB12, Pull::Up);
    let perform_calib = calib_button.is_low();

    // Default values: 15% and 45% of 320
    // 320 * 0.15 = 48
    // 320 * 0.45 = 144
    let mut start_duty: u32 = 48;
    let mut end_duty: u32 = 144;
    let pwm_period = 320; // 8MHz / 25kHz

    if perform_calib {
        let mut buf_title: heapless::String<16> = heapless::String::new();

        let _ = write!(buf_title, "Calibrating");
        let buf_empty: heapless::String<16> = heapless::String::new();
        if let Err(_) = draw_ui(&mut display, &buf_title, &buf_empty) {
            defmt::warn!("Display failed during calib init");
        };

        // Stop pump and wait 5s to settle
        pump.set_duty_override(0);
        Timer::after_millis(5000).await;

        // Store calibration points: (duty_percent, raw_duty, rpm)
        let mut points: [(u8, u32, u32); 21] = [(0, 0, 0); 21];

        // Sweep up
        for (i, percent) in (0..=100).step_by(5).enumerate() {
            let duty = pwm_period * percent as u32 / 100;
            pump.set_duty_override(duty);

            // Display - Line 1: "XX% -> RPM"
            Timer::after_millis(1500).await;
            let rpm = RPM.load(Ordering::Relaxed);

            let mut buf_data: heapless::String<16> = heapless::String::new();
            let _ = write!(buf_data, "{}% -> {}", percent, rpm);
            let _ = draw_ui(&mut display, &buf_title, &buf_data);

            defmt::info!("Calib: {}% ({} ticks) -> {} RPM", percent, duty, rpm);

            if i < 21 {
                points[i] = (percent as u8, duty, rpm);
            }
        }

        // Smart Range Detection
        // 1. Find global Minimum RPM (The Valley)
        let mut min_rpm = u32::MAX;
        let mut min_idx = 0;

        for (i, p) in points.iter().enumerate() {
            if p.2 < min_rpm && p.2 > 0 {
                min_rpm = p.2;
                min_idx = i;
            }
        }

        // 2. Find Max RPM (Saturation)
        let mut max_rpm = 0;
        for p in points.iter() {
            if p.2 > max_rpm {
                max_rpm = p.2;
            }
        }

        // 3. Find Start (Min) and End (Max) Duty
        // Start is at the Valley (min_idx)
        // End is where we reach ~95% of Max RPM
        let mut max_idx = 20;
        let threshold = max_rpm * 95 / 100;

        for (i, p) in points.iter().enumerate() {
            if i >= min_idx && p.2 >= threshold {
                max_idx = i;
                break;
            }
        }

        // Start Algorithm: Find "Knee" (> min + 50)
        let mut start_idx = min_idx;
        for i in min_idx..points.len() {
            if points[i].2 > min_rpm + 50 {
                start_idx = i;
                break;
            }
        }

        // Potential Limits
        let s_duty = points[start_idx].1;
        let e_duty = points[max_idx].1;

        // Validate
        if s_duty < e_duty {
            start_duty = s_duty;
            end_duty = e_duty;

            let mut buf_rng: heapless::String<16> = heapless::String::new();
            let _ = write!(
                buf_rng,
                "Rng: {}-{}%",
                points[start_idx].0, points[max_idx].0
            );
            let _ = draw_ui(&mut display, &buf_title, &buf_rng);
            defmt::info!(
                "Detected Range: {}% - {}%",
                points[start_idx].0,
                points[max_idx].0
            );
        } else {
            let mut buf_fail: heapless::String<16> = heapless::String::new();
            let _ = write!(buf_fail, "Calib: Fail");
            let buf_empty: heapless::String<16> = heapless::String::new();
            let _ = draw_ui(&mut display, &buf_fail, &buf_empty);
            defmt::error!("Calibration Failed: Start >= End. Using defaults.");
            // Keep the initialized defaults
        }
        Timer::after_millis(4000).await;

        // Reset display after calibration
        display.clear(BinaryColor::Off).unwrap();
        display.flush().unwrap();
    } else {
        defmt::info!(
            "Skipping Calibration. Button not pressed. Using defaults: 15% (48) - 45% (144)"
        );
    }

    // Apply the limits (Either calculated or default)
    pump.set_duty_limits(start_duty, end_duty);

    // Set initial speed to 50% of the working range
    let mid_duty = (start_duty + end_duty) / 2 - (end_duty - start_duty) * 5 / 100;
    pump.set_duty_override(mid_duty);

    // Ensure display is clear before loop
    display.clear(BinaryColor::Off).unwrap();
    display.flush().unwrap();
    // --- End Startup Logic ---

    // Initialize filter with first reading
    let mut temp_filter: u16 = adc.read(&mut p.PA1).await;

    loop {
        pump.update();

        let now = embassy_time::Instant::now().as_millis() as u32;
        let last_pulse = LAST_PULSE_MS.load(Ordering::Relaxed);

        let current_rpm = if now.wrapping_sub(last_pulse) > 1000 {
            0
        } else {
            RPM.load(Ordering::Relaxed)
        };

        let pct = pump.get_duty_percentage();

        let mut buf_rpm: heapless::String<16> = heapless::String::new();
        let _ = write!(buf_rpm, "RPM:  {}", current_rpm);

        let mut buf_duty: heapless::String<16> = heapless::String::new();

        // Read Temp
        // Read Temp with EMA Filter
        let raw_temp = adc.read(&mut p.PA1).await;
        // EMA: y[n] = (alpha * x[n]) + (1 - alpha) * y[n-1]
        // Integer approx: (new + 7 * old) / 8
        temp_filter = ((raw_temp as u32 + 7 * temp_filter as u32) / 8) as u16;

        let temp_c = convert_to_celsius(temp_filter);

        let _ = write!(buf_duty, "D:{}% {}C", pct, temp_c);

        // Timeout set to 100ms for display update via blocking driver (handled by driver timeout)
        if let Err(_) = draw_ui(&mut display, &buf_rpm, &buf_duty) {
            defmt::warn!("Display Error! Re-initializing...");
            Timer::after_millis(500).await;
            if let Err(_) = display.init() {
                defmt::error!("Display Re-init failed configuration");
            } else {
                // Settling time for display controller after init
                Timer::after_millis(100).await;
            }
        }

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

const LINE_LEN: usize = 16; // line width in characters (unused in graphics mode)

// Helper to draw UI with 10x20 font
fn draw_ui<DI>(
    display: &mut Ssd1306<DI, DisplaySize128x64, BufferedGraphicsMode<DisplaySize128x64>>,
    line1: &str,
    line2: &str,
) -> Result<(), ()>
where
    DI: WriteOnlyDataCommand,
{
    display.clear(BinaryColor::Off).unwrap();

    let style = MonoTextStyle::new(&FONT_10X20, BinaryColor::On);

    // Line 1 at (0, 15) - Baseline Top
    Text::with_baseline(line1, Point::new(0, 0), style, Baseline::Top)
        .draw(display)
        .unwrap();

    // Line 2 at (0, 35)
    Text::with_baseline(line2, Point::new(0, 32), style, Baseline::Top)
        .draw(display)
        .unwrap();

    match display.flush() {
        Ok(_) => Ok(()),
        Err(_) => {
            defmt::warn!("Display flush failed");
            Err(())
        }
    }
}

fn convert_to_celsius(raw: u16) -> i32 {
    const B: f32 = 3950.0;
    const T0: f32 = 298.15;
    const R0: f32 = 10000.0;
    const R_PULLUP: f32 = 10000.0;
    const MAX_ADC: f32 = 4095.0;

    let val = raw as f32;
    // Avoid division by zero or log(0)
    if val >= MAX_ADC - 1.0 {
        return -99;
    }
    if val < 1.0 {
        return -99;
    }

    // Circuit: 3.3V -> 10k -> PA1 -> NTC -> GND
    // Vout = Vcc * Rntc / (Rpu + Rntc)
    // ADC = 4095 * Rntc / (Rpu + Rntc)
    // Rntc = Rpu * ADC / (4095 - ADC)
    let r_ntc = R_PULLUP * val / (MAX_ADC - val);

    // Beta formula: 1/T = 1/T0 + 1/B * ln(R/R0)
    let log_r = libm::logf(r_ntc / R0);
    let temp_k = 1.0 / ((1.0 / T0) + (1.0 / B) * log_r);
    let temp_c = temp_k - 273.15;

    temp_c as i32
}
