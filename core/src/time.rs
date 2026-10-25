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

impl core::fmt::Display for Millis {
    /// A length of time as a person reads it: `500 ms`, `10.5 s`, `14m 59s`,
    /// `1h 15m`.
    ///
    /// The raw millisecond count is exact but unreadable, and a log line is read
    /// at a glance - `deep-sleeping for 899498 ms` takes arithmetic, `14m 59s`
    /// does not. Exact values are still one `as_millis()` away when a number
    /// looks wrong.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let ms = self.0;

        if ms < 1_000 {
            return write!(f, "{ms} ms");
        }

        let secs = ms / 1_000;

        if secs < 60 {
            // One decimal: enough to see a settle delay or a sleep floor.
            return write!(f, "{}.{} s", secs, (ms % 1_000) / 100);
        }

        let (mins, secs) = (secs / 60, secs % 60);

        if mins < 60 {
            return match secs {
                0 => write!(f, "{mins}m"),
                _ => write!(f, "{mins}m {secs}s"),
            };
        }

        let (hours, mins) = (mins / 60, mins % 60);

        match mins {
            0 => write!(f, "{hours}h"),
            _ => write!(f, "{hours}h {mins}m"),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_duration_reads_the_way_a_person_says_it() {
        let cases = [
            (0, "0 ms"),
            (1, "1 ms"),
            (999, "999 ms"),
            (1_000, "1.0 s"),
            (10_500, "10.5 s"),
            (59_900, "59.9 s"),
            (60_000, "1m"),
            (899_498, "14m 59s"),
            (900_000, "15m"),
            (3_600_000, "1h"),
            (4_500_000, "1h 15m"),
        ];

        for (ms, expected) in cases {
            assert_eq!(
                Millis::from_millis(ms).to_string(),
                expected,
                "{ms} ms should read as {expected}"
            );
        }
    }
}
