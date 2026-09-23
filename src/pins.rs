//! XIAO ESP32-C6 soil moisture kit pin map and board tunables.
//!
//! Mirrors `app_priv.h` from the original C++ firmware. GPIO peripherals
//! themselves are typed per-pin in `esp-hal`, so they are taken directly off
//! `Peripherals` in `main.rs` (`peripherals.GPIO0`, etc.) rather than named
//! here; this module holds the tunables that would otherwise be magic
//! numbers.

use embassy_time::Duration;

/// 200 kHz excitation, 68% duty - same as the stock ESPHome firmware, and
/// what the LEDC timer's resolution caps out at for this frequency (80 MHz /
/// 200 kHz = 400 counts, so 8-bit duty resolution is the most this timer can
/// do here).
pub const SOIL_EXCITATION_FREQ_KHZ: u32 = 200;
pub const SOIL_EXCITATION_DUTY_PCT: u8 = 68;
pub const SOIL_EXCITATION_SETTLE: Duration = Duration::from_millis(300);

pub const SOIL_SAMPLE_COUNT: u32 = 10;
pub const SOIL_SAMPLE_GAP: Duration = Duration::from_millis(50);

pub const BATTERY_SAMPLE_COUNT: u32 = 5;
pub const BATTERY_SAMPLE_GAP: Duration = Duration::from_millis(20);

/// Default dry/wet calibration references (mV), ported from the original
/// firmware's Kconfig defaults. Overridden once a calibration is persisted.
pub const SOIL_CAL_DEFAULT_DRY_MV: i32 = 1600;
pub const SOIL_CAL_DEFAULT_WET_MV: i32 = 900;

/// A calibration needs the dry reference meaningfully above wet.
pub const SOIL_CAL_MIN_SPAN_MV: i32 = 200;

/// AA cell resting-voltage-to-percent endpoints (mV).
pub const BATTERY_EMPTY_MV: i32 = 1000;
pub const BATTERY_FULL_MV: i32 = 1500;

/// Only report a new moisture/battery reading when it moved enough to be a
/// real trend, so ADC jitter does not wake the Thread radio with a report.
pub const SOIL_REPORT_DELTA_PERCENT: u8 = 2;
pub const BATTERY_REPORT_DELTA_MV: i32 = 20;

/// Moisture classification thresholds carried over from the ESPHome
/// firmware's ref_dry (0.23) and ref_wet (0.58) fractions.
pub const MOISTURE_DRY_BELOW_PERCENT: u8 = 23;
pub const MOISTURE_NORMAL_ABOVE_PERCENT: u8 = 58;

pub const BUTTON_TRIPLE_CLICK_WINDOW: Duration = Duration::from_millis(400);
pub const BUTTON_FACTORY_RESET_HOLD: Duration = Duration::from_secs(10);
pub const BUTTON_DEBOUNCE: Duration = Duration::from_millis(20);
