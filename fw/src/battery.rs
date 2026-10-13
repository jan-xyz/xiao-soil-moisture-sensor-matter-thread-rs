//! The cell's resting voltage: an averaged ADC read.
//!
//! Like the probe, this decides nothing. The state-of-charge arithmetic and the
//! discharge trend are in the decision layer; this only produces a voltage.

use embassy_time::{Duration, Timer};
use esp_hal::analog::adc::{Adc, AdcCalCurve, AdcChannel, AdcPin};
use esp_hal::peripherals::ADC1;
use esp_hal::Async;

use soil_sensor_core::tuning::{BATTERY_SAMPLE_COUNT, BATTERY_SAMPLE_GAP};

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

    /// Averages `BATTERY_SAMPLE_COUNT` calibrated reads of the resting
    /// (unloaded) cell voltage, `BATTERY_SAMPLE_GAP` apart.
    pub async fn sample_mv(&mut self, adc: &mut Adc<'d, ADC1<'d>, Async>) -> u32 {
        let mut sum = 0u32;
        for _ in 0..BATTERY_SAMPLE_COUNT {
            sum += u32::from(adc.read_oneshot(&mut self.pin).await);
            Timer::after(Duration::from_millis(BATTERY_SAMPLE_GAP.as_millis())).await;
        }

        sum / BATTERY_SAMPLE_COUNT
    }
}
