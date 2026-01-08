CHIP = stm32f103c8

release:
	cargo flash --chip $(CHIP) --release

dev:
	cargo flash --chip $(CHIP)

reset:
	probe-rs reset --chip $(CHIP)

attach:
	probe-rs attach --chip $(CHIP) target/thumbv7m-none-eabi/debug/pump_ctrl

env:
	rustup default stable
	rustup target add thumbv7m-none-eabi
	cargo install flip-link
	cargo install probe-rs-tools --locked
