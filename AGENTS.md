# AGENTS.md

This file provides guidelines for AI agents working in the `pump_ctrl` embedded Rust project.

## Project Overview

**Language**: Rust 2021 edition
**Target**: STM32F103C8 (ARM Cortex-M3, 64KB Flash, 20KB RAM)
**Framework**: Embassy async framework (v0.9)
**Type**: `no_std` embedded systems project

## Build Commands

```bash
# Debug build
cargo build

# Release build with flash
make release
# or
cargo flash --chip stm32f103c8 --release

# Run on hardware (uses probe-rs runner)
cargo run

# Reset microcontroller
make reset
# or
probe-rs reset --chip stm32f103c8

# Attach debugger
make attach
# or
probe-rs attach --chip stm32f103c8 target/thumbv7m-none-eabi/debug/pump_ctrl
```

## Testing

**NO TESTS CURRENTLY EXIST** - This is a `no_std` embedded project.

For embedded Rust testing, create host-side tests in a separate package or use `#[cfg(test)]` with mocks.

To run cargo clippy:
```bash
cargo clippy --target thumbv7m-none-eabi
```

## Code Style Guidelines

### Imports

- Group imports logically: local modules → core/std → external crates → panic handlers
- Use `self::` for local modules, absolute paths for external crates
- Panic handlers use underscore alias: `use {defmt_rtt as _, panic_probe as _};`

```rust
use self::pump::PumpController;
use core::fmt::Write;
use embassy_executor::Spawner;
use {defmt_rtt as _, panic_probe as _};
```

### Formatting

- Indentation: 4 spaces
- Line length: Keep under 100 characters (no strict enforcement)
- Trailing commas: Use in multi-line struct initializations and function calls
- Quote style: Double quotes only
- Attributes: Place on separate lines before items

```rust
#[embassy_executor::main]
async fn main(spawner: Spawner) {
    Self {
        pwm,
        enc,
        min_duty,
        max_duty,
        duty,
        target_percent: 0,
    }
}
```

### Type Conventions

- Function parameters: Explicit types
- Lifetimes: Explicit where needed (`'a`, `'b`)
- Generic bounds: Descriptive names
- Type inference: Use inside function bodies
- Embedded: Use `heapless::String<N>` for fixed-capacity strings

```rust
pub struct PumpController<'a, T: GeneralInstance4Channel, E: GeneralInstance4Channel> {
    pwm: SimplePwm<'a, T>,
    enc: Qei<'a, E>,
}

let mut buf: heapless::String<16> = heapless::String::new();
```

### Naming Conventions

- Variables: `snake_case` (e.g., `tach_timer_freq`, `target_duty`)
- Functions/Methods: `snake_case` (e.g., `update()`, `make_line()`)
- Types/Structs/Enums: `PascalCase` (e.g., `PumpController`)
- Constants: `UPPER_SNAKE_CASE` (e.g., `TIM_MAX`, `LINE_LEN`)
- Acronyms: Treat as regular words (e.g., `I2c`, `PwmPin`)
- Modules: `snake_case` files

### Timer Overflow Handling

**CRITICAL**: All timers on STM32F103C8 are 16-bit (max value 65535 / 0xFFFF)

- At 1 MHz timer frequency: wraps every 65.5 ms
- For low RPM measurements (<916 RPM), multiple overflows occur between captures
- Track overflows using `overflow.rs` module
- Enable update interrupt: `embassy_stm32::pac::timer::TIM3.dier().modify(|w| w.set_uie(true))`
- Check overflow flag after each capture: `check_and_count_overflow()`
- Calculate total ticks: `(overflow_count * 65536) + (current - previous)`

```rust
// Enable update interrupt for overflow tracking
embassy_stm32::interrupt::free(|_| {
    let regs = embassy_stm32::pac::timer::TIM3;
    regs.dier().modify(|w| w.set_uie(true));
});

// Check and count overflows after each capture
check_and_count_overflow();

// Calculate ticks with overflow
let ticks = calculate_ticks_with_overflow(prev, curr, overflow_count);
```

### Timer Overflow Handling

**CRITICAL**: All timers on STM32F103C8 are 16-bit (max value 65535 / 0xFFFF)

- At 1 MHz timer frequency: wraps every 65.5 ms
- For low RPM measurements (<916 RPM), multiple overflows occur between captures
- Track overflows using `overflow.rs` module
- Enable update interrupt: `regs.dier().modify(|w| w.set_uie(true))`
- Check overflow flag after each capture: `check_and_count_overflow()`
- Calculate total ticks: `(overflow_count * 65536) + (current - previous)`

```rust
// Enable update interrupt for overflow tracking
embassy_stm32::interrupt::free(|_| {
    let regs = embassy_stm32::timer::low_level::regs::Tim3::regs();
    regs.dier().modify(|w| w.set_uie(true));
});

// Check and count overflows after each capture
check_and_count_overflow();

// Calculate ticks with overflow
let ticks = calculate_ticks_with_overflow(prev, curr, overflow_count);
```

### Additional Patterns

- Numeric literals: Use underscores for readability (`8_000_000`, `25_000`)
- Hex constants: For register values (`0xFFFF`)
- Use `Option<T>` for nullable values
- Attribute for dev: `#![allow(unused_imports, dead_code, unused_variables)]`
