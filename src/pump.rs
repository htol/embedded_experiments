use defmt::info;
use embassy_stm32::timer::qei::Qei;
use embassy_stm32::timer::simple_pwm::SimplePwm;
use embassy_stm32::timer::GeneralInstance4Channel;

/// Контроллер помпы с управлением от энкодера
pub struct PumpController<'a, T: GeneralInstance4Channel, E: GeneralInstance4Channel> {
    pwm: SimplePwm<'a, T>,
    enc: Qei<'a, E>,
    min_duty: u32,
    max_duty: u32,
    duty: u32,
    target_percent: u32,
    last_pos: u16,
}

impl<'a, T: GeneralInstance4Channel, E: GeneralInstance4Channel> PumpController<'a, T, E> {
    pub fn new(pwm: SimplePwm<'a, T>, enc: Qei<'a, E>) -> Self {
        let max_duty = pwm.max_duty_cycle().into();
        let min_duty = max_duty / 4;
        let duty = max_duty / 2;
        let last_pos = enc.count();
        info!("max: {}, min: {}", max_duty, min_duty);
        Self {
            pwm,
            enc,
            min_duty,
            max_duty,
            duty,
            target_percent: 0,
            last_pos,
        }
    }

    pub fn last_enc(&self) -> u16 {
        self.last_pos
    }

    pub fn update(&mut self) {
        let enc_now = self.enc.count();
        let delta = (self.last_pos as i16) - (enc_now as i16);
        self.last_pos = enc_now;

        let target_duty = self.duty.saturating_add_signed(delta as i32);
        let target_duty = target_duty.clamp(self.min_duty, self.max_duty);
        self.duty = target_duty;

        info!(
            "enc_now: {}, delta: {}, target %: {}, t.duty: {} duty: {}",
            enc_now, delta, self.target_percent, target_duty, self.duty,
        );

        self.pwm.ch1().set_duty_cycle(self.duty as u16);
    }

    pub fn duty(&self) -> u32 {
        self.duty
    }

    pub fn max_duty(&self) -> u32 {
        self.max_duty
    }

    pub fn target_percent(&self) -> u32 {
        self.target_percent
    }
}
