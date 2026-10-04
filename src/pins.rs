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

/// `BatTimeRemaining` estimate: one voltage sample per
/// [`BATTERY_HISTORY_INTERVAL_SECS`], [`BATTERY_HISTORY_LEN`] samples (24 h)
/// in the fit. No estimate until the samples span
/// [`BATTERY_TIME_REMAINING_MIN_SPAN_SECS`] and the fitted voltage fell by
/// [`BATTERY_TIME_REMAINING_MIN_DROP_MV`], because a shorter or flatter trend is
/// mostly ADC noise. A rise of [`BATTERY_REPLACED_RISE_MV`] means a new cell.
pub const BATTERY_HISTORY_INTERVAL_SECS: u32 = 900;
pub const BATTERY_HISTORY_LEN: usize = 96;
pub const BATTERY_TIME_REMAINING_MIN_SPAN_SECS: u32 = 4 * 3_600;
pub const BATTERY_TIME_REMAINING_MIN_DROP_MV: i32 = 10;
pub const BATTERY_REPLACED_RISE_MV: i32 = 100;

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

/// SED idle data-poll period for **SIT** ("Standard") mode, i.e. while no
/// fabric has a registered ICD client and its controller expects
/// responsiveness. The Matter spec caps the SIT slow-poll interval at 15 s. It
/// is also the advertised Session Idle Interval (`BASIC_INFO.sii`) while
/// operating as a SIT: the controller waits that long before it retries a
/// message, and a message reaches this node only when it polls its parent.
pub const THREAD_SIT_POLL_PERIOD_MS: u32 = 15_000;

/// SED idle data-poll period for **LIT** ("Battery Saver") mode, i.e. while
/// every fabric has a registered ICD client, so every controller waits for a
/// Check-In and long silence is expected. This is the advertised
/// `BASIC_INFO.sii` and the ICD `IdleModeDuration`; the Thread driver sizes the
/// OpenThread child timeout from it.
pub const THREAD_LIT_POLL_PERIOD_MS: u32 = 900_000;

/// SED active data-poll period (ms), used while the ICD is in active mode -
/// after boot, after any Matter message, on an ICD `StayActiveRequest` and on a
/// local button press - so the controller's response lands promptly. It is also
/// the advertised Session Active Interval (`BASIC_INFO.sai`).
pub const THREAD_ACTIVE_POLL_PERIOD_MS: u32 = 5_000;

/// How long the ICD stays in active mode (and at
/// [`THREAD_ACTIVE_POLL_PERIOD_MS`]) after boot, an idle-to-active transition
/// or any Matter message: `ICD_MODE.active_mode_duration_ms` and
/// `active_mode_threshold_ms`.
pub const THREAD_ACTIVE_HOLD_MS: u32 = 30_000;

/// How many ICD Check-Ins may be sent between two flash writes of the Check-In
/// counter: the persisted boundary jumps this far ahead of the live counter, and
/// equally far forward across a restart.
pub const ICD_COUNTER_EPOCH: u32 = 1_000;

/// Background sampling cadence (seconds). The measurement loop wakes the CPU
/// this often regardless of the radio's poll schedule.
pub const SAMPLE_PERIOD_SECS: u32 = 30;

/// esp-radio 802.15.4 receive-queue depth (frames buffered before drops, logged
/// as "Receive queue full"). esp-radio's own default of 10 is too small for
/// OpenThread's RX bursts; the `openthread` crate raises it to 50. A sleepy end
/// device gets its downlink in bursts after each data poll, so give it more
/// headroom. Each queued frame is a ~130-byte heap buffer held until reset, so
/// 200 caps the worst case at roughly 26 KB.
pub const THREAD_RX_QUEUE_SIZE: usize = 200;

/// How long a boot must run before it counts in the persisted Matter
/// `RebootCount` (General Diagnostics cluster).
pub const REBOOT_COUNT_HEALTHY_AFTER: Duration = Duration::from_secs(60);
