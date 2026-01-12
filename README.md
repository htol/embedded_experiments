# Embedded Experiments - Pump Controller

This project implements a firmware for an STM32F103 (Blue Pill) based pump controller. It features PWM control, tachometer feedback, temperature monitoring, and an OLED display interface.

## TODO

- [ ] Add temperature feedback
- [ ] Make display menu for configuration and fine tuning

## Hardware Requirements

- **Microcontroller**: STM32F103C8T6 (Blue Pill)
- **Display**: SSD1306 128x64 OLED (I2C)
- **Input**: Rotary Encoder with Push Button
- **Sensors**:
  - NTC 10k Temperature Sensor (10k Pull-up)
  - Tachometer Signal (from DDC Pump)
- **Actuator**: PWM Controlled Pump

## Pin Configuration

| Component | Pin | Function | Notes |
| :--- | :--- | :--- | :--- |
| **Pump PWM** | `PA0` | TIM2_CH1 | 25kHz PWM Output |
| **NTC Sensor** | `PA1` | ADC1_IN1 | Connect with 10k Pull-up to 3.3V |
| **Tachometer** | `PA6` | TIM3_CH1 | Input Capture (Pull-up) |
| **Encoder A** | `PA8` | TIM1_CH1 | Quadrature Encoder |
| **Encoder B** | `PA9` | TIM1_CH2 | Quadrature Encoder |
| **Display SCL** | `PB10` | I2C2_SCL | |
| **Display SDA** | `PB11` | I2C2_SDA | |
| **Calib Button**| `PB12` | GPIO Input | **Active Low** (GND to Activate) |
| **Status LED** | `PC13` | GPIO Output | Heartbeat blink |

## Features

### Hybrid Calibration

The system supports two startup modes controlled by the **PB12** pin:

1. **Default Mode (Normal Operation)**:
    - **Action**: Ensure `PB12` is **released** (High) during boot.
    - **Behavior**: System starts immediately using safe default limits (15% - 45% PWM duty).
    - **Initial Speed**: Sets pump to 50% of the working range.

2. **Calibration Mode**:
    - **Action**: Hold `PB12` to **GND** (Low) during boot/reset.
    - **Behavior**: System performs a full 0-100% sweep to characterize the pump's RPM response. It detects the minimum startup duty and maximum saturation point to define the optimal working range for the session.

### Control Loop

- **Encoder**: Adjusts target duty cycle within the defined limits (Hardware or Software limits).
- **Display**: Shows real-time RPM, Duty Cycle %, and Temperature (°C).
- **Protection**: Hardware Limits prevent stalling (min duty) or saturation (max duty).

## Build and Run

### Prerequisites

- Rust toolchain (`rustup target add thumbv7m-none-eabi`)
- `probe-rs` for flashing/debugging
- `cargo-binutils` (optional)

### Commands

**Run (Debug)**:

```bash
cargo run
# OR
make dev
```

**Build Release**:

```bash
cargo build --release
```

**Attach to running target**:

```bash
make attach
```
