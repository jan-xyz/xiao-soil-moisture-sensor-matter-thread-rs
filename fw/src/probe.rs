//! The capacitive soil probe: an excited, averaged ADC read.
//!
//! Nothing here decides anything. It drives the excitation signal, settles,
//! averages, and hands back a voltage - which is exactly the "read the ambient
//! value" capability the decision layer asks for.

use embassy_time::{Duration, Timer};
use esp_hal::analog::adc::{Adc, AdcCalCurve, AdcChannel, AdcPin};
use esp_hal::ledc::channel::{Channel, ChannelIFace};
use esp_hal::peripherals::ADC1;
use esp_hal::Async;

use soil_sensor_core::tuning::{
    SOIL_EXCITATION_DUTY_PCT, SOIL_EXCITATION_SETTLE, SOIL_SAMPLE_COUNT, SOIL_SAMPLE_GAP,
};

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
    /// `SOIL_SAMPLE_COUNT` calibrated reads, excitation off. Takes roughly
    /// settle + `SOIL_SAMPLE_COUNT * SOIL_SAMPLE_GAP`, but - unlike a blocking
    /// ADC read - yields to the executor the whole time instead of
    /// busy-spinning, so it never starves the Thread/Matter stack sharing this
    /// single-core executor.
    pub async fn sample_mv(&mut self, adc: &mut Adc<'d, ADC1<'d>, Async>) -> u32 {
        self.excitation.set_duty(SOIL_EXCITATION_DUTY_PCT).ok();
        Timer::after(Duration::from_millis(SOIL_EXCITATION_SETTLE.as_millis())).await;

        let mut sum = 0u32;
        for _ in 0..SOIL_SAMPLE_COUNT {
            sum += u32::from(adc.read_oneshot(&mut self.pin).await);
            Timer::after(Duration::from_millis(SOIL_SAMPLE_GAP.as_millis())).await;
        }

        self.excitation.set_duty(0).ok();

        sum / SOIL_SAMPLE_COUNT
    }
}
