//! The pin map, and the handful of numbers that describe the board rather than
//! the device's behaviour.
//!
//! This is the device layer's equivalent of the decision layer's tuning module:
//! if a number would change when the hardware changed, it belongs here; if it
//! would change when the *product* changed, it belongs in
//! [`soil_sensor_core::tuning`].
//!
//! The peripherals themselves are typed per-pin by `esp-hal`, so they are taken
//! directly off `Peripherals` in `main.rs` rather than named here. This module
//! exists to record which pin is which, so that the answer is in one place.
//!
//! | Pin | Function |
//! |---|---|
//! | 0 | Battery voltage ADC |
//! | 1 | Soil probe ADC |
//! | 2 | User button (active low, deep-sleep wake source) |
//! | 3 | RF switch enable (drive low) |
//! | 14 | Antenna select (drive high, external U.FL) |
//! | 18 | Yellow LED |
//! | 19 | Green LED |
//! | 20 | Red LED |
//! | 21 | Soil probe excitation PWM |

// Note: the button is on GPIO2 and not on the module's BOOT pin because a
// deep-sleep wake source has to be one of GPIO0..7. Deep sleep powers down the
// high-performance GPIO peripheral, and only those pads have the low-power path
// that survives it, so moving the button off them would silently cost the device
// its only user input.

/// 802.15.4 receive-queue depth (frames buffered before drops, logged as
/// "Receive queue full"). The radio driver's own default of 10 is too small for
/// the bursts a sleepy end device gets after each data poll; the `openthread`
/// crate raises it to 50.
///
/// A power of two on purpose. The queue is one `VecDeque` of ~130-byte frames
/// that grows by doubling, inside the receive interrupt, and never shrinks. A
/// limit of 200 grows it to 256 slots: one ~33 KB block, allocated at whatever
/// moment a burst arrives, while the old ~17 KB block is still held for the
/// copy. 128 caps the queue at one ~17 KB block, and a failed allocation there
/// is a panic in an interrupt handler.
pub const THREAD_RX_QUEUE_SIZE: usize = 128;
