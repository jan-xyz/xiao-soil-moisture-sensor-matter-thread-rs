//! The simulation layer: a scripted world, and the driver that walks boots
//! through it.
//!
//! This is a peer of the device layer, not a test fixture. The firmware and the
//! simulation both call the same boot loop, over ports with the same meaning;
//! only the composition root chooses between them. What changes is that here the
//! clock moves only when the code asks, and a reboot is something the driver can
//! perform as many times as it likes.
//!
//! What it cannot do is worth being explicit about: it does not model the
//! silicon, and it does not model radio timing. Numbers that come out of it are
//! a model, not a measurement. See `ARCHITECTURE.md`.

pub mod energy;
pub mod world;

pub use energy::Energy;
pub use world::{BootRecord, Report, Script, World};
