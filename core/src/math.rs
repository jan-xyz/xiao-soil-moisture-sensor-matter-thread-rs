//! The plain arithmetic, and the one piece of state it keeps.
//!
//! Nothing here reads a clock, a pin or a byte of flash, so all of it is
//! exercised on the host. That is the pattern the rest of the decision layer
//! follows: if a calculation can be written without the world, it is.

/// Linear interpolation between the wet (low mV) and dry (high mV)
/// soil-probe calibration references, clamped to 0-100%.
pub fn soil_moisture_percent(mv: i32, dry_mv: i32, wet_mv: i32) -> u8 {
    let span = dry_mv - wet_mv;
    if span <= 0 {
        return 0;
    }
    let pct = 100 * (dry_mv - mv) / span;
    pct.clamp(0, 100) as u8
}

/// The charge left in a cell at resting voltage `mv`, in permille, read off
/// `curve`: (millivolts, permille) points in rising order, joined by straight
/// lines and clamped at both ends.
pub fn battery_permille(mv: i32, curve: &[(i32, u32)]) -> u32 {
    let (Some(&(lowest_mv, lowest)), Some(&(highest_mv, highest))) = (curve.first(), curve.last())
    else {
        return 0;
    };
    if mv <= lowest_mv {
        return lowest;
    }
    if mv >= highest_mv {
        return highest;
    }

    curve
        .windows(2)
        .find(|pair| mv < pair[1].0)
        .map(|pair| {
            let ((from_mv, from), (to_mv, to)) = (pair[0], pair[1]);
            let span_mv = i64::from(to_mv) - i64::from(from_mv);
            if span_mv <= 0 {
                return from;
            }
            let rise = i64::from(to) - i64::from(from);
            let offset = (i64::from(from) + rise * (i64::from(mv) - i64::from(from_mv)) / span_mv)
                .clamp(0, i64::from(u32::MAX));
            offset as u32
        })
        .unwrap_or(highest)
}

/// Whether a new reading moved far enough from the last *reported* one to be
/// reported. Comparing against the last report rather than the previous sample
/// lets a slow trend accumulate until it crosses `min_delta`, while jitter
/// below `min_delta` still does not wake the radio. The first reading always
/// reports.
pub fn should_report(last_reported: Option<u32>, current: u32, min_delta: u32) -> bool {
    match last_reported {
        Some(last_reported) => last_reported.abs_diff(current) >= min_delta,
        None => true,
    }
}

/// A ring buffer of battery voltage samples, used to estimate how long the
/// cell lasts from the rate at which its voltage falls.
///
/// The estimate is a least-squares line through the samples, so ADC jitter
/// between single readings averages out. An alkaline cell does not discharge
/// linearly: the estimate follows the recent trend, not the full curve.
///
/// The layout is deliberately plain data - no `Option`, no tuples, no
/// padding - so the binary can place it in RTC RAM and have it survive the
/// deep-sleep reboots a LIT device makes. The binary is the only place that
/// knows about that; see `battery.rs`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DischargeHistory<const N: usize> {
    sample_s: [u32; N],
    sample_mv: [i32; N],
    /// Number of valid samples, `0..=N`.
    len: u32,
    /// Where the next sample goes.
    next: u32,
    /// The time of the last recorded sample; only meaningful while
    /// `has_last` is non-zero.
    last_s: u32,
    has_last: u32,
    interval_s: u32,
    replaced_rise_mv: i32,
}

impl<const N: usize> DischargeHistory<N> {
    /// Records at most one sample per `interval_s`. A voltage more than
    /// `replaced_rise_mv` above the highest recorded sample means a new cell,
    /// and clears the history.
    pub const fn new(interval_s: u32, replaced_rise_mv: i32) -> Self {
        Self {
            sample_s: [0; N],
            sample_mv: [0; N],
            len: 0,
            next: 0,
            last_s: 0,
            has_last: 0,
            interval_s,
            replaced_rise_mv,
        }
    }

    /// How many samples the trend currently holds.
    pub fn len(&self) -> usize {
        (self.len as usize).min(N)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn push(&mut self, now_s: u32, mv: i32) {
        // Clamped rather than trusted: this can live in RTC RAM, where a reset
        // that outran the guard in `battery.rs` can leave any bit pattern.
        let len = (self.len as usize).min(N);

        // A clock that went backwards restarted while the history survived -
        // a reset that kept RTC RAM but not the RTC timer. The old samples are
        // on another time base, and a fit across both would be nonsense.
        let clock_restarted = self.has_last != 0 && now_s < self.last_s;

        let highest_mv = self.sample_mv[..len].iter().copied().max();
        let new_cell = matches!(highest_mv, Some(highest_mv) if mv > highest_mv.saturating_add(self.replaced_rise_mv));

        if clock_restarted || new_cell {
            self.len = 0;
            self.next = 0;
            self.has_last = 0;
        }

        if self.has_last != 0 && now_s.saturating_sub(self.last_s) < self.interval_s {
            return;
        }

        let next = (self.next as usize) % N;
        self.sample_s[next] = now_s;
        self.sample_mv[next] = mv;
        self.next = (next as u32 + 1) % N as u32;
        self.len = ((self.len as usize).min(N) + 1).min(N) as u32;
        self.last_s = now_s;
        self.has_last = 1;
    }

    /// Seconds until the fitted voltage line reaches `empty_mv`. `None` until
    /// the samples span at least `min_span_s` and the fitted line fell by at
    /// least `min_drop_mv` over that span, because a shorter or flatter trend
    /// is mostly noise.
    pub fn time_remaining_s(
        &self,
        empty_mv: i32,
        min_span_s: u32,
        min_drop_mv: i32,
    ) -> Option<u32> {
        let len = (self.len as usize).min(N);
        let times = &self.sample_s[..len];
        let first_s = times.iter().copied().min()?;
        let last_s = times.iter().copied().max()?;
        let span_s = last_s - first_s;
        if span_s < min_span_s || span_s == 0 {
            return None;
        }

        // `i128`, because the history can live in RTC RAM and hold any bit
        // pattern: with `u32` timestamps and `i32` voltages, no product or sum
        // below can leave it, while `dt * dt` alone already overflows `i64`
        // for a span past ~3e9 s - a panic on every boot of a debug build.
        let n = len as i128;
        let mean_t = times.iter().map(|&t| i128::from(t - first_s)).sum::<i128>() / n;
        let mean_mv = self.sample_mv[..len]
            .iter()
            .map(|&mv| i128::from(mv))
            .sum::<i128>()
            / n;
        let (cov, var) = (0..len).fold((0i128, 0i128), |(cov, var), i| {
            let dt = i128::from(times[i] - first_s) - mean_t;
            (
                cov + dt * (i128::from(self.sample_mv[i]) - mean_mv),
                var + dt * dt,
            )
        });
        if var == 0 {
            return None;
        }

        let drop_mv = -cov * i128::from(span_s) / var;
        if drop_mv < i128::from(min_drop_mv) {
            return None;
        }

        let last_fit_mv = mean_mv + cov * (i128::from(last_s - first_s) - mean_t) / var;
        let remaining_s = (last_fit_mv - i128::from(empty_mv)).max(0) * var / -cov;

        Some(remaining_s.clamp(0, i128::from(u32::MAX)) as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct SoilTestCase {
        name: &'static str,
        input_mv: i32,
        input_dry_mv: i32,
        input_wet_mv: i32,
        expected: u8,
    }

    #[test]
    fn test_soil_moisture_percent() {
        let test_cases = vec![
            SoilTestCase {
                name: "at dry reference reads 0%",
                input_mv: 1600,
                input_dry_mv: 1600,
                input_wet_mv: 900,
                expected: 0,
            },
            SoilTestCase {
                name: "at wet reference reads 100%",
                input_mv: 900,
                input_dry_mv: 1600,
                input_wet_mv: 900,
                expected: 100,
            },
            SoilTestCase {
                name: "midpoint reads 50%",
                input_mv: 1250,
                input_dry_mv: 1600,
                input_wet_mv: 900,
                expected: 50,
            },
            SoilTestCase {
                name: "wetter than wet reference clamps to 100%",
                input_mv: 500,
                input_dry_mv: 1600,
                input_wet_mv: 900,
                expected: 100,
            },
            SoilTestCase {
                name: "drier than dry reference clamps to 0%",
                input_mv: 2000,
                input_dry_mv: 1600,
                input_wet_mv: 900,
                expected: 0,
            },
            SoilTestCase {
                name: "invalid (non-positive) span reads 0%",
                input_mv: 1200,
                input_dry_mv: 900,
                input_wet_mv: 1600,
                expected: 0,
            },
        ];

        for SoilTestCase {
            name,
            input_mv,
            input_dry_mv,
            input_wet_mv,
            expected,
        } in test_cases
        {
            let result = soil_moisture_percent(input_mv, input_dry_mv, input_wet_mv);
            assert_eq!(result, expected, "Failed case: '{name}'");
        }
    }

    struct BatteryTestCase {
        name: &'static str,
        input_mv: i32,
        input_curve: &'static [(i32, u32)],
        expected: u32,
    }

    #[test]
    fn test_battery_permille() {
        const CURVE: &[(i32, u32)] = &[(1000, 0), (1200, 200), (1500, 1000)];

        let test_cases = vec![
            BatteryTestCase {
                name: "at the lowest point reads its charge",
                input_mv: 1000,
                input_curve: CURVE,
                expected: 0,
            },
            BatteryTestCase {
                name: "at the highest point reads its charge",
                input_mv: 1500,
                input_curve: CURVE,
                expected: 1000,
            },
            BatteryTestCase {
                name: "at a point in between reads that point",
                input_mv: 1200,
                input_curve: CURVE,
                expected: 200,
            },
            BatteryTestCase {
                name: "between two points reads the line that joins them",
                input_mv: 1350,
                input_curve: CURVE,
                expected: 600,
            },
            BatteryTestCase {
                name: "below the curve clamps to the lowest point",
                input_mv: 800,
                input_curve: CURVE,
                expected: 0,
            },
            BatteryTestCase {
                name: "above the curve clamps to the highest point",
                input_mv: 1800,
                input_curve: CURVE,
                expected: 1000,
            },
            BatteryTestCase {
                name: "an empty curve reads empty",
                input_mv: 1200,
                input_curve: &[],
                expected: 0,
            },
            BatteryTestCase {
                name: "a vertical step in the curve reads its top",
                input_mv: 1200,
                input_curve: &[(1000, 0), (1200, 300), (1200, 500), (1500, 1000)],
                expected: 500,
            },
        ];

        for BatteryTestCase {
            name,
            input_mv,
            input_curve,
            expected,
        } in test_cases
        {
            let result = battery_permille(input_mv, input_curve);
            assert_eq!(result, expected, "Failed case: '{name}'");
        }
    }

    const HOUR_S: u32 = 3_600;

    struct DischargeTestCase {
        name: &'static str,
        input_samples: &'static [(u32, i32)],
        input_interval_s: u32,
        input_replaced_rise_mv: i32,
        input_empty_mv: i32,
        input_min_span_s: u32,
        input_min_drop_mv: i32,
        expected: Option<u32>,
    }

    #[test]
    fn test_discharge_history_time_remaining() {
        let test_cases = vec![
            DischargeTestCase {
                name: "no samples has no estimate",
                input_samples: &[],
                input_interval_s: 0,
                input_replaced_rise_mv: 100,
                input_empty_mv: 1000,
                input_min_span_s: HOUR_S,
                input_min_drop_mv: 10,
                expected: None,
            },
            DischargeTestCase {
                name: "steady 10 mV per hour fall extrapolates to empty",
                input_samples: &[(0, 1400), (HOUR_S, 1390), (2 * HOUR_S, 1380)],
                input_interval_s: 0,
                input_replaced_rise_mv: 100,
                input_empty_mv: 1000,
                input_min_span_s: HOUR_S,
                input_min_drop_mv: 10,
                expected: Some(38 * HOUR_S),
            },
            DischargeTestCase {
                name: "jitter around a steady fall averages out",
                input_samples: &[
                    (0, 1405),
                    (HOUR_S, 1385),
                    (2 * HOUR_S, 1385),
                    (3 * HOUR_S, 1365),
                ],
                input_interval_s: 0,
                input_replaced_rise_mv: 100,
                input_empty_mv: 1000,
                input_min_span_s: HOUR_S,
                input_min_drop_mv: 10,
                expected: Some(110_100),
            },
            DischargeTestCase {
                name: "span shorter than the minimum has no estimate",
                input_samples: &[(0, 1400), (HOUR_S, 1300)],
                input_interval_s: 0,
                input_replaced_rise_mv: 100,
                input_empty_mv: 1000,
                input_min_span_s: 2 * HOUR_S,
                input_min_drop_mv: 10,
                expected: None,
            },
            DischargeTestCase {
                name: "drop smaller than the minimum has no estimate",
                input_samples: &[(0, 1400), (HOUR_S, 1398), (2 * HOUR_S, 1396)],
                input_interval_s: 0,
                input_replaced_rise_mv: 100,
                input_empty_mv: 1000,
                input_min_span_s: HOUR_S,
                input_min_drop_mv: 10,
                expected: None,
            },
            DischargeTestCase {
                name: "rising voltage has no estimate",
                input_samples: &[(0, 1380), (HOUR_S, 1390), (2 * HOUR_S, 1400)],
                input_interval_s: 0,
                input_replaced_rise_mv: 100,
                input_empty_mv: 1000,
                input_min_span_s: HOUR_S,
                input_min_drop_mv: 10,
                expected: None,
            },
            DischargeTestCase {
                name: "fitted voltage below empty reads zero",
                input_samples: &[(0, 1020), (HOUR_S, 1000), (2 * HOUR_S, 980)],
                input_interval_s: 0,
                input_replaced_rise_mv: 100,
                input_empty_mv: 1000,
                input_min_span_s: HOUR_S,
                input_min_drop_mv: 10,
                expected: Some(0),
            },
            DischargeTestCase {
                name: "samples closer than the interval are skipped",
                input_samples: &[
                    (0, 1400),
                    (HOUR_S / 2, 1000),
                    (HOUR_S, 1390),
                    (2 * HOUR_S, 1380),
                ],
                input_interval_s: HOUR_S,
                input_replaced_rise_mv: 100,
                input_empty_mv: 1000,
                input_min_span_s: HOUR_S,
                input_min_drop_mv: 10,
                expected: Some(38 * HOUR_S),
            },
            DischargeTestCase {
                name: "a new cell clears the old history",
                input_samples: &[
                    (0, 1100),
                    (HOUR_S, 1050),
                    (2 * HOUR_S, 1500),
                    (3 * HOUR_S, 1490),
                    (4 * HOUR_S, 1480),
                ],
                input_interval_s: 0,
                input_replaced_rise_mv: 100,
                input_empty_mv: 1000,
                input_min_span_s: HOUR_S,
                input_min_drop_mv: 10,
                expected: Some(48 * HOUR_S),
            },
            DischargeTestCase {
                name: "a full buffer drops the oldest samples",
                input_samples: &[
                    (0, 1500),
                    (HOUR_S, 1400),
                    (2 * HOUR_S, 1390),
                    (3 * HOUR_S, 1380),
                    (4 * HOUR_S, 1370),
                ],
                input_interval_s: 0,
                input_replaced_rise_mv: 200,
                input_empty_mv: 1000,
                input_min_span_s: HOUR_S,
                input_min_drop_mv: 10,
                expected: Some(37 * HOUR_S),
            },
            DischargeTestCase {
                name: "a clock that restarted clears the old history",
                input_samples: &[
                    (10 * HOUR_S, 1400),
                    (11 * HOUR_S, 1390),
                    (0, 1300),
                    (HOUR_S, 1290),
                    (2 * HOUR_S, 1280),
                ],
                input_interval_s: 0,
                input_replaced_rise_mv: 100,
                input_empty_mv: 1000,
                input_min_span_s: HOUR_S,
                input_min_drop_mv: 10,
                expected: Some(28 * HOUR_S),
            },
            DischargeTestCase {
                name: "values no cell produces do not overflow",
                input_samples: &[(0, i32::MAX), (u32::MAX / 2, 0), (u32::MAX, i32::MIN)],
                input_interval_s: 0,
                input_replaced_rise_mv: i32::MAX,
                input_empty_mv: 1000,
                input_min_span_s: HOUR_S,
                input_min_drop_mv: 10,
                expected: Some(0),
            },
        ];

        for DischargeTestCase {
            name,
            input_samples,
            input_interval_s,
            input_replaced_rise_mv,
            input_empty_mv,
            input_min_span_s,
            input_min_drop_mv,
            expected,
        } in test_cases
        {
            let mut history = DischargeHistory::<4>::new(input_interval_s, input_replaced_rise_mv);
            for &(now_s, mv) in input_samples {
                history.push(now_s, mv);
            }

            let result =
                history.time_remaining_s(input_empty_mv, input_min_span_s, input_min_drop_mv);
            assert_eq!(result, expected, "Failed case: '{name}'");
        }
    }

    struct ShouldReportTestCase {
        name: &'static str,
        input_last_reported: Option<u32>,
        input_current: u32,
        input_min_delta: u32,
        expected: bool,
    }

    #[test]
    fn test_should_report() {
        let test_cases = vec![
            ShouldReportTestCase {
                name: "first reading always reports",
                input_last_reported: None,
                input_current: 40,
                input_min_delta: 2,
                expected: true,
            },
            ShouldReportTestCase {
                name: "jitter below the delta does not report",
                input_last_reported: Some(40),
                input_current: 41,
                input_min_delta: 2,
                expected: false,
            },
            ShouldReportTestCase {
                name: "a slow fall reports once it reaches the delta",
                input_last_reported: Some(40),
                input_current: 38,
                input_min_delta: 2,
                expected: true,
            },
            ShouldReportTestCase {
                name: "a rise reports once it reaches the delta",
                input_last_reported: Some(1400),
                input_current: 1420,
                input_min_delta: 20,
                expected: true,
            },
        ];

        for ShouldReportTestCase {
            name,
            input_last_reported,
            input_current,
            input_min_delta,
            expected,
        } in test_cases
        {
            let result = should_report(input_last_reported, input_current, input_min_delta);
            assert_eq!(result, expected, "Failed case: '{name}'");
        }
    }
}
