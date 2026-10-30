//! Every number the device's behaviour depends on.
//!
//! These are decisions, not hardware facts, which is why they live in the
//! decision layer and not next to the peripherals. The pin map is the other way
//! round and lives with the device layer.
//!
//! Retuning the device should mean editing this file and nothing else; if a
//! change here needs a change anywhere else, the number is in the wrong place.

use crate::time::Millis;

// --- Soil probe -------------------------------------------------------------

/// Excitation frequency and duty of the probe's drive signal. The stock
/// firmware uses the same pair, and the resolution the hardware timer can reach
/// at this frequency is what fixes the duty step.
pub const SOIL_EXCITATION_FREQ_KHZ: u32 = 200;
pub const SOIL_EXCITATION_DUTY_PCT: u8 = 68;

/// How long the probe is left to settle after the drive signal changes before
/// the first reading is taken.
pub const SOIL_EXCITATION_SETTLE: Millis = Millis::from_millis(300);

/// Readings averaged per measurement, and the gap between them. Averaging is
/// what makes a single sample trustworthy enough to report; the gap lets the
/// analog front end recover between reads.
pub const SOIL_SAMPLE_COUNT: u32 = 10;
pub const SOIL_SAMPLE_GAP: Millis = Millis::from_millis(50);

/// Dry/wet reference voltages, in millivolts. Overridden once a calibration has
/// been stored.
pub const SOIL_CAL_DEFAULT_DRY_MV: i32 = 1600;
pub const SOIL_CAL_DEFAULT_WET_MV: i32 = 900;

/// A calibration is only accepted when dry sits this far above wet; anything
/// closer is a mis-clamped probe, not a calibration.
pub const SOIL_CAL_MIN_SPAN_MV: i32 = 200;

/// The two references the calibration flow captures, and the hold time of each
/// half of the flow.
pub const SOIL_CAL_HOLD: Millis = Millis::from_secs(10);
pub const SOIL_CAL_BLINK_PERIOD: Millis = Millis::from_millis(500);

/// How far a moisture reading must move before it is worth a report. Below
/// this, the reading is converter noise and reporting it would wake the radio
/// for nothing.
pub const SOIL_REPORT_DELTA_PERCENT: u8 = 2;

/// Moisture classification thresholds for the status LEDs.
pub const MOISTURE_DRY_BELOW_PERCENT: u8 = 23;
pub const MOISTURE_NORMAL_ABOVE_PERCENT: u8 = 58;

// --- Battery ----------------------------------------------------------------

pub const BATTERY_SAMPLE_COUNT: u32 = 5;
pub const BATTERY_SAMPLE_GAP: Millis = Millis::from_millis(20);

/// Resting-voltage endpoints of the state-of-charge estimate.
pub const BATTERY_EMPTY_MV: i32 = 1000;
pub const BATTERY_FULL_MV: i32 = 1500;

/// The trend that estimates remaining life: one sample per interval, this many
/// samples retained, and the minimum span and fall before an estimate is
/// offered at all. A shorter or flatter trend is mostly noise.
pub const BATTERY_HISTORY_INTERVAL_SECS: u32 = 900;
pub const BATTERY_HISTORY_LEN: usize = 96;
pub const BATTERY_TIME_REMAINING_MIN_SPAN_SECS: u32 = 4 * 3_600;
pub const BATTERY_TIME_REMAINING_MIN_DROP_MV: i32 = 10;

/// A rise of this much above the highest sample seen means a new cell, and
/// clears the trend.
pub const BATTERY_REPLACED_RISE_MV: i32 = 100;

/// How far the resting voltage must move before it is worth a report.
pub const BATTERY_REPORT_DELTA_MV: i32 = 20;

// --- Button -----------------------------------------------------------------

/// A press shorter than this is a contact glitch, not a gesture.
pub const BUTTON_DEBOUNCE: Millis = Millis::from_millis(20);

/// Presses closer together than this count as one gesture.
pub const BUTTON_TRIPLE_CLICK_WINDOW: Millis = Millis::from_millis(400);

/// How long the button has to be held to ask for a factory reset.
pub const BUTTON_FACTORY_RESET_HOLD: Millis = Millis::from_secs(10);

/// How long the device stays reachable after a button press that woke it. A
/// wake is a reboot, so the press that caused it is still in progress when the
/// firmware starts: this is the room the rest of the gesture needs.
pub const BUTTON_WAKE_HOLD: Millis = Millis::from_millis(30_000);

// --- Sampling cadence -------------------------------------------------------

/// Background sampling period while the device is awake.
pub const SAMPLE_PERIOD: Millis = Millis::from_secs(30);

/// How long a counted boot - any boot except a wake from sleep, per the spec's
/// RebootCount clause (core spec §11.12.6.2) - must run before its count is
/// written back, so that a boot loop does not spend flash writes.
pub const REBOOT_COUNT_HEALTHY_AFTER: Millis = Millis::from_secs(60);

// --- Reachability -----------------------------------------------------------

/// The slowest poll a Short Idle Time device may use, in milliseconds. A SIT
/// device has to stay reachable within this, which is why it can never sleep
/// for long.
pub const SIT_SLOW_POLL_MAX_MS: u32 = 15_000;

/// The slow poll used while operating as a SIT, in milliseconds.
pub const SIT_POLL_PERIOD_MS: u32 = 15_000;

/// The idle period of Long Idle Time operation, in milliseconds. This is both
/// the advertised idle interval and the length of a deep sleep, because a
/// device that sleeps does not poll.
pub const LIT_POLL_PERIOD_MS: u32 = 900_000;

/// The same, in whole seconds, as the protocol's idle-mode duration.
pub const LIT_IDLE_MODE_DURATION_SECS: u32 = LIT_POLL_PERIOD_MS / 1_000;

/// The fast poll used while active, in milliseconds, advertised as the session
/// active interval. Short, because the active window is short: the device has
/// to catch a peer's reply before it sleeps again, and the radio time this
/// costs is nothing next to being awake at all. It is also the granularity of
/// the check-in exchange: every round trip of it waits up to one poll period,
/// and a slow poll here stretches the boot's awake time directly.
pub const ACTIVE_POLL_PERIOD_MS: u32 = 300;

/// How long a boot stays active before it is allowed to go idle. Every
/// received message extends the window by [`ACTIVE_THRESHOLD_MS`] on its
/// own, and that threshold sits at the spec's LIT floor of 5 s: the tail
/// after the last exchange is fixed by the spec either way. The hold itself
/// is only the controller-latency insurance: the check-in exchange keeps the
/// window alive, and the observed controller connects within a couple of
/// seconds of the device becoming reachable - with one observed outlier at
/// ~14 s, which costs that cycle's subscription and recovers on the next
/// wake-up's check-in.
pub const ACTIVE_HOLD: Millis = Millis::from_millis(2_000);

/// How much extra active time every protocol message buys. The protocol's
/// minimum for a long-idle device, and what keeps the device up while a peer is
/// mid-exchange with it.
pub const ACTIVE_THRESHOLD_MS: u16 = 5_000;

/// How long the device is kept active for the interactive calibration flow,
/// which takes far longer than an active window.
pub const CALIBRATION_ACTIVE_HOLD: Millis = Millis::from_millis(60_000);

/// How many check-ins may pass between two writes of the check-in counter: the
/// stored boundary jumps this far ahead, and equally far across a restart.
pub const CHECK_IN_COUNTER_EPOCH: u32 = 1_000;

// --- Recovery ---------------------------------------------------------------

/// How long the boot loop may go without waiting for anything before the
/// hardware watchdog resets the chip. Nothing else ever resets a hung chip, and
/// a hung chip keeps its radio on until the cell is empty.
///
/// The boot loop waits at least once per [`SAMPLE_PERIOD`]. The longest stretch
/// between two waits is a reading or the protocol stack's startup, and both
/// take seconds, so four sample periods leave a wide margin.
pub const WATCHDOG_TIMEOUT: Millis = Millis::from_secs(120);

const _: () = assert!(WATCHDOG_TIMEOUT.as_millis() >= 2 * SAMPLE_PERIOD.as_millis());

// --- Sleeping ---------------------------------------------------------------

/// How long to wait after going idle before sleeping, so that the persistence
/// that the active period caused has time to reach flash.
pub const SLEEP_SETTLE: Millis = Millis::from_millis(500);

/// Never sleep for less than this: the reboot a wake-up costs is worth more
/// than a shorter sleep saves.
pub const SLEEP_MIN: Millis = Millis::from_millis(1_000);
