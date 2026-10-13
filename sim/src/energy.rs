//! What a simulated run costs, in the only terms that matter to a battery.
//!
//! The currents are the chip's, from the datasheet, and the duty cycle is what
//! the simulation actually did. The result is therefore a model: it says what
//! the *decisions* cost, and nothing about the board around the chip.

use soil_sensor_core::tuning;

/// Typical current while the CPU is running with the radio off, in milliamps.
/// The datasheet's "modem-sleep, CPU idle, 160 MHz, peripheral clocks disabled"
/// figure; a real board with its regulators and flash sits above it.
pub const AWAKE_MA: f64 = 17.0;

/// Typical current in deep sleep, in microamps. Only the low-power timer and
/// memory stay powered.
pub const ASLEEP_UA: f64 = 7.0;

/// A representative AA cell, in milliamp-hours. Capacity depends on the drain
/// rate and the cut-off, so this is a reference figure rather than a guarantee.
pub const AA_CAPACITY_MAH: f64 = 2500.0;

#[derive(Clone, Copy, Debug)]
pub struct Energy {
    pub awake_ms: u64,
    pub asleep_ms: u64,
}

impl Energy {
    pub fn from_world(world: &super::World) -> Self {
        Self {
            awake_ms: world.awake_ms(),
            asleep_ms: world.asleep_ms(),
        }
    }

    pub fn total_ms(&self) -> u64 {
        self.awake_ms + self.asleep_ms
    }

    /// The fraction of the time the device was awake.
    pub fn awake_fraction(&self) -> f64 {
        match self.total_ms() {
            0 => 0.0,
            total => self.awake_ms as f64 / total as f64,
        }
    }

    /// Average current over the run, in microamps.
    pub fn average_ua(&self) -> f64 {
        match self.total_ms() {
            0 => 0.0,
            total => {
                let awake_ms = self.awake_ms as f64;
                let asleep_ms = self.asleep_ms as f64;
                (awake_ms * AWAKE_MA * 1_000.0 + asleep_ms * ASLEEP_UA) / total as f64
            }
        }
    }

    /// How long a cell of `capacity_mah` would last at that average current.
    pub fn life_years(&self, capacity_mah: f64) -> f64 {
        let average_ua = self.average_ua();
        if average_ua <= 0.0 {
            return f64::INFINITY;
        }

        capacity_mah * 1_000.0 / average_ua / (24.0 * 365.0)
    }

    /// A one-line summary, for the command-line tool.
    pub fn summary(&self) -> String {
        let days = self.total_ms() as f64 / 86_400_000.0;
        format!(
            "{:.1} simulated days | awake {:.2} h ({:.3}%) | asleep {:.1} h | \
             average {:.1} uA | ~{:.1} years on a {:.0} mAh AA",
            days,
            self.awake_ms as f64 / 3_600_000.0,
            self.awake_fraction() * 100.0,
            self.asleep_ms as f64 / 3_600_000.0,
            self.average_ua(),
            self.life_years(AA_CAPACITY_MAH),
            AA_CAPACITY_MAH,
        )
    }
}

/// The energy of one wake-up, as the model sees it - useful for sanity checks
/// against the tuning.
pub fn awake_ms_per_boot() -> u64 {
    tuning::ACTIVE_HOLD.as_millis()
}
