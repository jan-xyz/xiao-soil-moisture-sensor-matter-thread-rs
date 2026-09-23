//! Capacitive soil probe: 200 kHz PWM excitation + averaged ADC read.
//!
//! Replaces `soil_probe.cpp`. The dry/wet calibration flow itself lives in
//! [`crate::calibration`]; mV-to-percent conversion is
//! [`soil_sensor_core::soil_moisture_percent`] (split out for host testing -
//! this module depends on `esp-hal`, which does not build for the host).

use embassy_time::Timer;
use esp_hal::analog::adc::{Adc, AdcCalCurve, AdcChannel, AdcPin};
use esp_hal::ledc::channel::{Channel, ChannelIFace};
use esp_hal::peripherals::ADC1;
use esp_hal::Blocking;

use crate::pins::{SOIL_EXCITATION_DUTY_PCT, SOIL_EXCITATION_SETTLE, SOIL_SAMPLE_COUNT, SOIL_SAMPLE_GAP};

pub struct SoilProbe<'d, PIN> {
    excitation: Channel<'d, esp_hal::ledc::LowSpeed>,
    pin: AdcPin<PIN, ADC1<'d>, AdcCalCurve<ADC1<'d>>>,
}

impl<'d, PIN> SoilProbe<'d, PIN>
where
    PIN: AdcChannel,
{
    pub fn new(
        excitation: Channel<'d, esp_hal::ledc::LowSpeed>,
        pin: AdcPin<PIN, ADC1<'d>, AdcCalCurve<ADC1<'d>>>,
    ) -> Self {
        Self { excitation, pin }
    }

    /// One full excited measurement: excitation on, settle, average
    /// `SOIL_SAMPLE_COUNT` calibrated reads, excitation off. Blocks for
    /// roughly settle + `SOIL_SAMPLE_COUNT * SOIL_SAMPLE_GAP`.
    pub async fn sample_mv(&mut self, adc: &mut Adc<'d, ADC1<'d>, Blocking>) -> Option<u32> {
        self.excitation.set_duty(SOIL_EXCITATION_DUTY_PCT).ok();
        Timer::after(SOIL_EXCITATION_SETTLE).await;

        let mut sum = 0u32;
        let mut valid = 0u32;
        for _ in 0..SOIL_SAMPLE_COUNT {
            if let Ok(mv) = nb::block!(adc.read_oneshot(&mut self.pin)) {
                sum += u32::from(mv);
                valid += 1;
            }
            Timer::after(SOIL_SAMPLE_GAP).await;
        }

        self.excitation.set_duty(0).ok();

        sum.checked_div(valid)
    }
}
