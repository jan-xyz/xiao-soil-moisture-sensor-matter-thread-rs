//! Time as the decision layer sees it.
//!
//! The decision layer never reads a clock. It is told what time it is, and it
//! asks to be woken at a deadline; both are values of the types here, measured
//! from the last power-on reset. That is what lets a simulated boot run in
//! microseconds and produce exactly the same answers as a real one.

use core::ops::Add;

/// A span of time, in milliseconds.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default, Hash)]
pub struct Millis(pub u64);

impl Millis {
    pub const ZERO: Self = Self(0);

    pub const fn from_millis(ms: u64) -> Self {
        Self(ms)
    }

    pub const fn from_secs(s: u64) -> Self {
        Self(s * 1000)
    }

    pub const fn as_millis(self) -> u64 {
        self.0
    }

    pub const fn as_secs(self) -> u64 {
        self.0 / 1000
    }

    pub const fn saturating_sub(self, other: Self) -> Self {
        Self(self.0.saturating_sub(other.0))
    }

    /// The larger of the two, as a duration helper for tuning floors.
    pub const fn max(self, other: Self) -> Self {
        if self.0 >= other.0 {
            self
        } else {
            other
        }
    }
}

impl Add for Millis {
    type Output = Self;

    fn add(self, rhs: Self) -> Self {
        Self(self.0.saturating_add(rhs.0))
    }
}

/// A point in time, in milliseconds since the last power-on reset.
///
/// Deliberately *not* a wall clock: a device that deep-sleeps has no continuous
/// wall clock, and behaviour that depended on one would be untestable and, more
/// importantly, wrong.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default, Hash)]
pub struct Instant(pub u64);

impl Instant {
    pub const ZERO: Self = Self(0);

    pub const fn from_millis(ms: u64) -> Self {
        Self(ms)
    }

    pub const fn from_secs(s: u64) -> Self {
        Self(s * 1000)
    }

    pub const fn as_millis(self) -> u64 {
        self.0
    }

    pub const fn saturating_add(self, d: Millis) -> Self {
        Self(self.0.saturating_add(d.0))
    }

    /// How long ago `earlier` was. Zero if `earlier` is in the future, so that
    /// a caller that has just been handed a stale instant cannot underflow.
    pub const fn saturating_since(self, earlier: Self) -> Millis {
        Millis(self.0.saturating_sub(earlier.0))
    }

    pub const fn saturating_sub(self, d: Millis) -> Self {
        Self(self.0.saturating_sub(d.0))
    }
}

impl Add<Millis> for Instant {
    type Output = Self;

    fn add(self, rhs: Millis) -> Self {
        self.saturating_add(rhs)
    }
}
