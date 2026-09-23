//! Dry/wet soil-probe calibration: interactive flow + flash persistence.
//!
//! Replaces the calibration half of `soil_probe.cpp`/`button.cpp`. Stored in
//! its own flash partition (see `partitions.csv`), separate from Matter's
//! own fabric/session state - a Matter factory reset (10 s button hold)
//! re-commissions the device but should not force the user to recalibrate
//! the physical probe, and vice versa.

use core::ops::Range;

use embassy_time::{Duration, Timer};
use esp_hal::analog::adc::{Adc, AdcChannel};
use esp_hal::peripherals::ADC1;
use esp_hal::Blocking;
use sequential_storage::cache::NoCache;
use sequential_storage::map::{fetch_item, store_item};

use crate::flash::SharedFlash;
use crate::pins::{SOIL_CAL_DEFAULT_DRY_MV, SOIL_CAL_DEFAULT_WET_MV, SOIL_CAL_MIN_SPAN_MV};
use crate::soil_probe::SoilProbe;
use crate::status_led::{Color, LedCommand, LedSender};

const KEY_DRY_MV: u8 = 1;
const KEY_WET_MV: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalibrationError {
    /// The probe could not take a measurement (no valid ADC reads).
    SampleFailed,
    /// The dry and wet references were not far enough apart to be a real
    /// calibration (`dry_mv - wet_mv < SOIL_CAL_MIN_SPAN_MV`).
    SpanTooSmall,
    /// The new references could not be written to flash; the previous
    /// values remain in effect.
    PersistFailed,
}

pub struct Calibration<'a, 'd> {
    flash: SharedFlash<'a, 'd>,
    range: Range<u32>,
    dry_mv: i32,
    wet_mv: i32,
}

impl<'a, 'd> Calibration<'a, 'd> {
    /// Loads the persisted dry/wet references from `range`, falling back to
    /// the firmware defaults when nothing has been calibrated yet.
    pub async fn load(flash: SharedFlash<'a, 'd>, range: Range<u32>) -> Self {
        let mut buf = [0u8; 32];
        let mut cache = NoCache::new();

        let mut f = flash;
        let dry_mv = fetch_item::<u8, i32, _>(&mut f, range.clone(), &mut cache, &mut buf, &KEY_DRY_MV)
            .await
            .ok()
            .flatten()
            .unwrap_or(SOIL_CAL_DEFAULT_DRY_MV);
        let wet_mv = fetch_item::<u8, i32, _>(&mut f, range.clone(), &mut cache, &mut buf, &KEY_WET_MV)
            .await
            .ok()
            .flatten()
            .unwrap_or(SOIL_CAL_DEFAULT_WET_MV);

        Self { flash, range, dry_mv, wet_mv }
    }

    pub fn dry_mv(&self) -> i32 {
        self.dry_mv
    }

    pub fn wet_mv(&self) -> i32 {
        self.wet_mv
    }

    async fn persist(&mut self) -> Result<(), ()> {
        let mut buf = [0u8; 32];
        let mut cache = NoCache::new();

        store_item(&mut self.flash, self.range.clone(), &mut cache, &mut buf, &KEY_DRY_MV, &self.dry_mv)
            .await
            .map_err(|_| ())?;
        store_item(&mut self.flash, self.range.clone(), &mut cache, &mut buf, &KEY_WET_MV, &self.wet_mv)
            .await
            .map_err(|_| ())?;

        Ok(())
    }

    /// The interactive dry/wet calibration flow (blocking, ~20 s): red
    /// blink 10 s with the probe in dry air captures the dry reference,
    /// then green blink 10 s with the probe in water captures the wet
    /// reference. Persists to flash on success; the previous calibration
    /// stays in effect on any error.
    pub async fn run_flow<'probe, PIN>(
        &mut self,
        probe: &mut SoilProbe<'probe, PIN>,
        adc: &mut Adc<'probe, ADC1<'probe>, Blocking>,
        leds: LedSender<'_>,
    ) -> Result<(), CalibrationError>
    where
        PIN: AdcChannel,
    {
        const HOLD: Duration = Duration::from_secs(10);
        const BLINK_PERIOD: Duration = Duration::from_millis(500);

        leds.send(LedCommand::Blink { color: Color::Red, count: 10, period: BLINK_PERIOD }).await;
        Timer::after(HOLD).await;
        let dry_mv = probe.sample_mv(adc).await.ok_or(CalibrationError::SampleFailed)?;

        leds.send(LedCommand::Blink { color: Color::Green, count: 10, period: BLINK_PERIOD }).await;
        Timer::after(HOLD).await;
        let wet_mv = probe.sample_mv(adc).await.ok_or(CalibrationError::SampleFailed)?;

        if (dry_mv as i32) - (wet_mv as i32) < SOIL_CAL_MIN_SPAN_MV {
            return Err(CalibrationError::SpanTooSmall);
        }

        self.dry_mv = dry_mv as i32;
        self.wet_mv = wet_mv as i32;
        self.persist().await.map_err(|()| CalibrationError::PersistFailed)
    }
}
