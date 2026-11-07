//! The capability vocabulary: the only thing the decision layer and the device
//! (or simulation) layer share.
//!
//! Ports are named after what the layer above needs, never after what the layer
//! below is. There is no "ADC channel" here, only "read the ambient value";
//! there is no "flash page", only "load the calibration".
//!
//! Two of the methods exist to answer questions the decision layer must never
//! ask itself: [`Ports::now`] (what time is it) and [`Ports::next_event`] (which
//! of the things I am waiting for happened first). Putting the second one in
//! the port is deliberate: deciding which of a deadline, a button edge and a
//! protocol state change comes first is a property of the world, not a
//! decision, and it is exactly the point where a simulator has to behave like
//! hardware.

use crate::{
    led::LedCommand,
    time::{Instant, Millis},
    tuning,
};

/// One battery estimate, from one reading of the cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BatteryReading {
    /// The charge left, in half-percent units: 0-200 for 0-100%. That is the
    /// unit of the `BatPercentRemaining` attribute, so no resolution is lost
    /// on the way to it.
    pub half_percent: u8,
    /// The resting voltage of the cell the estimate comes from, in millivolts.
    pub rest_mv: u32,
}

/// Which way the button just moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonEdge {
    /// The button was pressed. It pulls the pin low.
    Falling,
    /// The button was released.
    Rising,
}

/// Why this boot happened. A deep sleep is a reboot, so this is the first thing
/// a boot knows about itself.
///
/// Only the distinction the behaviour needs crosses this boundary. The fine
/// taxonomy of reset causes is a reporting concern, answered by the General
/// Diagnostics cluster's `BootReason` attribute, and stays in the device layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WakeCause {
    /// Power-on, or any reset that is not a wake from deep sleep.
    Cold,
    /// The deep-sleep timer ran out.
    Timer,
    /// A button press woke the device from deep sleep.
    Button,
}

/// The ICD (Intermittently Connected Device) power mode of the protocol stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerMode {
    /// The stack polls fast, because an exchange is in progress or expected.
    Active,
    /// The stack polls slowly, and the device may sleep.
    Idle,
}

/// The ICD operating mode of the protocol stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperatingMode {
    /// Short Idle Time: no controller has registered for check-ins, so the
    /// device has to stay reachable within seconds.
    Sit,
    /// Long Idle Time: a controller has registered for check-ins, so the
    /// device may sleep for a whole idle period.
    Lit,
}

/// What the protocol stack currently thinks of the device, flattened into plain
/// values so the decision layer never names a protocol type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IcdState {
    /// Whether the stack is active or idle right now.
    pub power_mode: PowerMode,
    /// Whether the stack operates as a short or a long idle device.
    pub operating_mode: OperatingMode,
    /// The poll interval in effect right now, in milliseconds.
    pub poll_interval_ms: u32,
    /// When idle mode ends by itself, or `None` while active.
    pub idle_until: Option<Instant>,
}

/// What a waiting boot was woken by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// The deadline the decision layer asked for.
    Deadline,
    /// The button moved.
    Button(ButtonEdge),
    /// The protocol stack's view of the device changed.
    Icd,
}

/// How a boot ended. The boot itself never performs either of these; the
/// composition root does, because both are irreversible.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Power the MCU down for this many milliseconds.
    Sleep {
        /// How long to sleep, in milliseconds.
        for_ms: u64,
    },
    /// Erase what a person would expect a reset to erase, then restart.
    FactoryReset,
}

/// The two probe references a calibration captures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Calibration {
    /// The probe reading in dry air, in millivolts. The higher of the two.
    pub dry_mv: i32,
    /// The probe reading in water, in millivolts.
    pub wet_mv: i32,
}

impl Calibration {
    /// The firmware's built-in references, for a device nobody has calibrated.
    pub const DEFAULT: Self = Self {
        dry_mv: tuning::SOIL_CAL_DEFAULT_DRY_MV,
        wet_mv: tuning::SOIL_CAL_DEFAULT_WET_MV,
    };
}

/// Everything a boot's wake phase needs from the world: the waking press
/// resolved, the boot's one reading taken.
///
/// This is the phase that runs before the protocol stack exists, which is why
/// there is no way here to speak through it - see [`MatterPorts`] for the line
/// between the phases. Implemented twice: once over real peripherals, once
/// over a scripted world. Nothing else in the tree implements it, and that is
/// the point - the same [`run_wake`](crate::app::run_wake) drives both.
#[allow(async_fn_in_trait)]
pub trait Ports {
    /// Milliseconds since this boot began. Use for deadlines within a boot.
    fn now(&self) -> Instant;

    /// Wait for `d`, with nothing able to cut the wait short.
    async fn delay(&mut self, d: Millis);

    /// Wait until the deadline, a button edge, or a change in the protocol
    /// stack's view - whichever comes first.
    async fn next_event(&mut self, deadline: Instant) -> Event;

    /// Whether an edge is already waiting, without waiting for one. A boot
    /// drains this before it sleeps, so a press that arrived while it was busy
    /// cannot be mistaken for an idle device.
    fn try_next_button_edge(&mut self) -> Option<ButtonEdge>;

    /// Show `cmd` on the status LEDs, without waiting for it to finish.
    fn send_led(&mut self, cmd: LedCommand);

    /// One averaged reading of the soil probe, in millivolts. The device layer
    /// owns everything about how that is produced - driving the probe,
    /// settling, averaging.
    async fn read_soil_mv(&mut self) -> u32;

    /// One averaged reading of the cell's resting voltage, in millivolts.
    async fn read_battery_mv(&mut self) -> u32;

    /// What the protocol stack thinks of the device right now.
    fn icd(&self) -> IcdState;

    /// Ask the protocol stack to stay reachable, for `hold` or for the
    /// stack's own default window.
    fn request_active(&mut self, hold: Option<Millis>);

    /// Hand a soil moisture reading, in percent, to the protocol stack.
    fn report_soil(&mut self, percent: u8);

    /// `None` when there is no cell to estimate: the reading is too low to come
    /// from one.
    fn report_battery(&mut self, reading: Option<BatteryReading>);

    /// The stored calibration, or [`Calibration::DEFAULT`] when there is none.
    async fn load_calibration(&mut self) -> Calibration;

    /// `false` when the values could not be stored, in which case the previous
    /// calibration stays in effect.
    async fn store_calibration(&mut self, calibration: Calibration) -> bool;
}

/// The part of the ports that exists only once the protocol stack does.
///
/// A boot has two phases, and this trait is the line between them. The wake
/// phase - the waking press and the boot's one reading - runs before the stack
/// does, and a port set of that phase cannot reach the stack at all. What
/// crosses it today is one thing: the reboot count, whose storage lives behind
/// the protocol stack's own persistence.
#[allow(async_fn_in_trait)]
pub trait MatterPorts: Ports {
    /// Persist the reboot count through the protocol stack's storage.
    fn persist_reboot_count(&mut self);
}
