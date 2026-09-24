//! AA cell resting-voltage sampling.
//!
//! Replaces the resting-voltage half of `battery.cpp`. The C++ original also
//! measures load sag (internal-resistance health) by driving the LEDs as a
//! load; that's out of scope for this port (see plan), so this only reports
//! resting voltage. mV-to-percent conversion is
//! [`soil_sensor_core::battery_percent`] (split out for host testing - this
//! module depends on `esp-hal`, which does not build for the host).

use embassy_time::Timer;
use esp_hal::analog::adc::{Adc, AdcCalCurve, AdcChannel, AdcPin};
use esp_hal::peripherals::ADC1;
use esp_hal::Async;

use crate::pins::{BATTERY_SAMPLE_COUNT, BATTERY_SAMPLE_GAP};

pub struct Battery<'d, PIN> {
    pin: AdcPin<PIN, ADC1<'d>, AdcCalCurve<ADC1<'d>>>,
}

impl<'d, PIN> Battery<'d, PIN>
where
    PIN: AdcChannel,
{
    pub fn new(pin: AdcPin<PIN, ADC1<'d>, AdcCalCurve<ADC1<'d>>>) -> Self {
        Self { pin }
    }

    /// Averages `BATTERY_SAMPLE_COUNT` calibrated reads, `BATTERY_SAMPLE_GAP`
    /// apart, of the resting (unloaded) cell voltage. Yields to the executor
    /// between reads rather than busy-spinning - see `SoilProbe::sample_mv`.
    pub async fn sample_mv(&mut self, adc: &mut Adc<'d, ADC1<'d>, Async>) -> u32 {
        let mut sum = 0u32;
        for _ in 0..BATTERY_SAMPLE_COUNT {
            sum += u32::from(adc.read_oneshot(&mut self.pin).await);
            Timer::after(BATTERY_SAMPLE_GAP).await;
        }

        sum / BATTERY_SAMPLE_COUNT
    }
}
