//! The decision layer: what the device does, as pure state machines.
//!
//! Everything in this crate is either a plain calculation, a state machine that
//! consumes events and produces actions, or the vocabulary that connects the
//! two to the outside world ([`ports`]). Nothing here names a peripheral, a
//! protocol type or a clock, so all of it runs - and is tested - on the host,
//! with no hardware and no executor.
//!
//! The one entry point the firmware and the simulator share is
//! [`app::run_boot`]: it drives one boot to its end and says how that boot
//! ended. A deep sleep is a reboot, so "one boot" is the natural unit of
//! behaviour, and running many of them in sequence is what the simulator does.
//!
//! See `ARCHITECTURE.md` for the layering rules this crate exists to enforce.

#![cfg_attr(not(test), no_std)]

pub mod app;
pub mod button;
pub mod calibration;
pub mod led;
pub mod math;
pub mod ports;
pub mod recovery;
pub mod time;
pub mod tuning;

pub use math::{battery_permille, battery_time_remaining_s, should_report, soil_moisture_percent};
pub use time::{Instant, Millis};
