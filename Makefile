CHIP = stm32f103c8

release:
	cargo flash --chip $(CHIP) --release

reset:
	probe-rs reset --chip $(CHIP)

attach:
	probe-rs attach --chip $(CHIP) target/thumbv7m-none-eabi/debug/pump_ctrl
