//! What a simulated run costs, in the only terms that matter to a battery.
//!
//! The currents are measured on the hardware (a multimeter in series with
//! the cell: ~100 mA awake, 0.12 mA asleep), and the duty cycle is what the
//! simulation actually did - plus the protocol stack's share of a wake, a
//! model, because the simulation does not run the stack. The result is
//! therefore a model twice over: it says what the *decisions* cost, and what
//! the radio work around them is *assumed* to cost.
//!
//! Both currents are battery-side, so the boost converter's overhead is
//! inside the awake figure.

use std::time::Duration;

/// Measured with a multimeter in series with the cell: ~100 mA while the
/// device is awake and online. That is the *battery-side* figure - the
/// carrier's boost converter from the 1.5 V cell draws ~100 mA to deliver
/// ~38 mA at the 3.3 V rail, where the Thread radio, the BLE commissioning
/// peripheral and the CPU run concurrently. Far above the datasheet's
/// "modem-sleep, CPU idle" 17 mA, which is why the awake window - not the
/// deep sleep - dominates the budget.
pub const AWAKE_MA: f64 = 100.0;

/// Measured with a multimeter in series with the cell: 0.12 mA in deep
/// sleep. The datasheet's figure for the chip alone is 7 uA; the rest is the
/// carrier board around it (boost converter, charger path, sense divider).
/// Worth hunting: at the 6-hour idle the awake duty is ~0.05%, so this floor
/// is most of the average now.
pub const ASLEEP_UA: f64 = 120.0;

/// A representative AA cell, in milliamp-hours. Capacity depends on the drain
/// rate and the cut-off, so this is a reference figure rather than a guarantee.
pub const AA_CAPACITY_MAH: f64 = 2500.0;

/// The protocol stack's share of one wake-up, in milliseconds: everything the
/// simulation cannot see because it does not run the stack - the stack's boot,
/// the Thread attach, the CASE resumption, the subscription's priming report
/// and the final persist.
///
/// Measured on the hardware, from a debug-logged wake at the reachability
/// tuning of that measurement (150 ms fast poll, 1 s active threshold, a
/// 30-minute idle - the wake's length does not depend on the idle period):
/// 7.55 s awake end to end, of which the simulation's own accounting covers
/// the decision layer's ~2.4 s (the wake-phase measurement and the
/// active-window bookkeeping), so this is the stack's remaining share. A
/// calibration or a button wake runs longer; charging every boot the same is
/// the conservative side of wrong.
pub const PROTOCOL_AWAKE_MS_PER_BOOT: u64 = 5_150;

/// Milliseconds as `f64`. `Duration` does the conversion, because a plain
/// `as` cast from `u64` can round, and clippy rejects it for that.
fn ms_f64(ms: u64) -> f64 {
    Duration::from_millis(ms).as_secs_f64() * 1_000.0
}

/// How a run split into awake and asleep time.
#[derive(Clone, Copy, Debug)]
pub struct Energy {
    /// Awake time, including the protocol stack's assumed share of each boot.
    pub awake_ms: u64,
    /// Time in deep sleep.
    pub asleep_ms: u64,
}

impl Energy {
    /// The energy of everything `world` has run so far.
    #[must_use]
    pub fn from_world(world: &super::World) -> Self {
        Self {
            awake_ms: world.awake_ms() + PROTOCOL_AWAKE_MS_PER_BOOT * world.boot_count() as u64,
            asleep_ms: world.asleep_ms(),
        }
    }

    /// The length of the run.
    #[must_use]
    pub fn total_ms(&self) -> u64 {
        self.awake_ms + self.asleep_ms
    }

    /// The fraction of the time the device was awake.
    #[must_use]
    pub fn awake_fraction(&self) -> f64 {
        match self.total_ms() {
            0 => 0.0,
            total => ms_f64(self.awake_ms) / ms_f64(total),
        }
    }

    /// Average current over the run, in microamps.
    #[must_use]
    pub fn average_ua(&self) -> f64 {
        match self.total_ms() {
            0 => 0.0,
            total => {
                let awake_ms = ms_f64(self.awake_ms);
                let asleep_ms = ms_f64(self.asleep_ms);
                (awake_ms * AWAKE_MA * 1_000.0 + asleep_ms * ASLEEP_UA) / ms_f64(total)
            }
        }
    }

    /// How long a cell of `capacity_mah` would last at that average current.
    #[must_use]
    pub fn life_years(&self, capacity_mah: f64) -> f64 {
        let average_ua = self.average_ua();
        if average_ua <= 0.0 {
            return f64::INFINITY;
        }

        capacity_mah * 1_000.0 / average_ua / (24.0 * 365.0)
    }

    /// A one-line summary, for the command-line tool.
    #[must_use]
    pub fn summary(&self) -> String {
        let days = ms_f64(self.total_ms()) / 86_400_000.0;
        format!(
            "{:.1} simulated days | awake {:.2} h ({:.3}%) | asleep {:.1} h | \
             average {:.1} uA | ~{:.1} years on a {:.0} mAh AA",
            days,
            ms_f64(self.awake_ms) / 3_600_000.0,
            self.awake_fraction() * 100.0,
            ms_f64(self.asleep_ms) / 3_600_000.0,
            self.average_ua(),
            self.life_years(AA_CAPACITY_MAH),
            AA_CAPACITY_MAH,
        )
    }
}
