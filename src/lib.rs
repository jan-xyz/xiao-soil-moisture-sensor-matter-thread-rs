//! Pure calculation logic, split out from the hardware-dependent binary.
//!
//! The binary is `#![no_std]`/`#![no_main]` and depends on `esp-hal`, which
//! does not build for the host target - so anything worth unit-testing
//! under `cargo test` on the host lives here instead, in a lib crate the
//! binary depends on implicitly (same package, standard Cargo lib+bin
//! linkage). `no_std` only when not under `cargo test`, so the host test
//! harness (which needs `std`) still works.
#![cfg_attr(not(test), no_std)]

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

/// Linear state-of-charge estimate between `empty_mv` and `full_mv`, clamped
/// to 0-100%.
pub fn battery_percent(mv: i32, empty_mv: i32, full_mv: i32) -> u8 {
    let span = full_mv - empty_mv;
    if span <= 0 {
        return 0;
    }
    let pct = 100 * (mv - empty_mv) / span;
    pct.clamp(0, 100) as u8
}

/// A ring buffer of battery voltage samples, used to estimate how long the
/// cell lasts from the rate at which its voltage falls.
///
/// The estimate is a least-squares line through the samples, so ADC jitter
/// between single readings averages out. An alkaline cell does not discharge
/// linearly: the estimate follows the recent trend, not the full curve.
pub struct DischargeHistory<const N: usize> {
    samples: [(u32, i32); N],
    len: usize,
    next: usize,
    last_s: Option<u32>,
    interval_s: u32,
    replaced_rise_mv: i32,
}

impl<const N: usize> DischargeHistory<N> {
    /// Records at most one sample per `interval_s`. A voltage more than
    /// `replaced_rise_mv` above the highest recorded sample means a new cell,
    /// and clears the history.
    pub const fn new(interval_s: u32, replaced_rise_mv: i32) -> Self {
        Self {
            samples: [(0, 0); N],
            len: 0,
            next: 0,
            last_s: None,
            interval_s,
            replaced_rise_mv,
        }
    }

    pub fn push(&mut self, now_s: u32, mv: i32) {
        let highest_mv = self.samples[..self.len].iter().map(|&(_, mv)| mv).max();
        if matches!(highest_mv, Some(highest_mv) if mv > highest_mv + self.replaced_rise_mv) {
            self.len = 0;
            self.next = 0;
            self.last_s = None;
        }

        if matches!(self.last_s, Some(last_s) if now_s.saturating_sub(last_s) < self.interval_s) {
            return;
        }

        self.samples[self.next] = (now_s, mv);
        self.next = (self.next + 1) % N;
        self.len = (self.len + 1).min(N);
        self.last_s = Some(now_s);
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
        let samples = &self.samples[..self.len];
        let first_s = samples.iter().map(|&(t, _)| t).min()?;
        let last_s = samples.iter().map(|&(t, _)| t).max()?;
        let span_s = last_s - first_s;
        if span_s < min_span_s || span_s == 0 {
            return None;
        }

        let n = samples.len() as i64;
        let mean_t = samples
            .iter()
            .map(|&(t, _)| i64::from(t - first_s))
            .sum::<i64>()
            / n;
        let mean_mv = samples.iter().map(|&(_, mv)| i64::from(mv)).sum::<i64>() / n;
        let (cov, var) = samples.iter().fold((0i64, 0i64), |(cov, var), &(t, mv)| {
            let dt = i64::from(t - first_s) - mean_t;
            (cov + dt * (i64::from(mv) - mean_mv), var + dt * dt)
        });
        if var == 0 {
            return None;
        }

        let drop_mv = -cov * i64::from(span_s) / var;
        if drop_mv < i64::from(min_drop_mv) {
            return None;
        }

        let last_fit_mv = mean_mv + cov * (i64::from(last_s - first_s) - mean_t) / var;
        let remaining_s = (last_fit_mv - i64::from(empty_mv)).max(0) * var / -cov;

        Some(remaining_s.clamp(0, i64::from(u32::MAX)) as u32)
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
        input_empty_mv: i32,
        input_full_mv: i32,
        expected: u8,
    }

    #[test]
    fn test_battery_percent() {
        let test_cases = vec![
            BatteryTestCase {
                name: "at empty reference reads 0%",
                input_mv: 1000,
                input_empty_mv: 1000,
                input_full_mv: 1500,
                expected: 0,
            },
            BatteryTestCase {
                name: "at full reference reads 100%",
                input_mv: 1500,
                input_empty_mv: 1000,
                input_full_mv: 1500,
                expected: 100,
            },
            BatteryTestCase {
                name: "midpoint reads 50%",
                input_mv: 1250,
                input_empty_mv: 1000,
                input_full_mv: 1500,
                expected: 50,
            },
            BatteryTestCase {
                name: "below empty clamps to 0%",
                input_mv: 800,
                input_empty_mv: 1000,
                input_full_mv: 1500,
                expected: 0,
            },
            BatteryTestCase {
                name: "above full clamps to 100%",
                input_mv: 1800,
                input_empty_mv: 1000,
                input_full_mv: 1500,
                expected: 100,
            },
            BatteryTestCase {
                name: "invalid (non-positive) span reads 0%",
                input_mv: 1200,
                input_empty_mv: 1500,
                input_full_mv: 1000,
                expected: 0,
            },
        ];

        for BatteryTestCase {
            name,
            input_mv,
            input_empty_mv,
            input_full_mv,
            expected,
        } in test_cases
        {
            let result = battery_percent(input_mv, input_empty_mv, input_full_mv);
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
}
