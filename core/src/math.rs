//! The plain arithmetic.
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

/// How long `permille` of a cell of `capacity_mah` lasts at an average draw
/// of `average_ua`, in seconds.
pub fn battery_time_remaining_s(permille: u32, capacity_mah: u32, average_ua: u32) -> u32 {
    // 1 mAh is 3,600,000,000 nC, and nC / uA is milliseconds. `u128`, because
    // no product of these inputs can leave it.
    let left_nc = u128::from(capacity_mah) * 3_600_000_000 * u128::from(permille) / 1000;
    let ms = left_nc / u128::from(average_ua.max(1));
    (ms / 1000).min(u128::from(u32::MAX)) as u32
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

    struct TimeRemainingTestCase {
        name: &'static str,
        input_permille: u32,
        input_capacity_mah: u32,
        input_average_ua: u32,
        expected: u32,
    }

    #[test]
    fn test_battery_time_remaining_s() {
        let test_cases = vec![
            TimeRemainingTestCase {
                name: "a full cell lasts its capacity over the current",
                input_permille: 1000,
                input_capacity_mah: 2000,
                input_average_ua: 1000,
                expected: 2000 * 3600,
            },
            TimeRemainingTestCase {
                name: "a cell at a tenth lasts a tenth as long",
                input_permille: 100,
                input_capacity_mah: 2000,
                input_average_ua: 1000,
                expected: 200 * 3600,
            },
            TimeRemainingTestCase {
                name: "an empty cell lasts nothing",
                input_permille: 0,
                input_capacity_mah: 2000,
                input_average_ua: 1000,
                expected: 0,
            },
            TimeRemainingTestCase {
                name: "no current does not divide by zero",
                input_permille: 1000,
                input_capacity_mah: 1,
                input_average_ua: 0,
                expected: 3_600_000,
            },
            TimeRemainingTestCase {
                name: "a time past the attribute's range clamps",
                input_permille: u32::MAX,
                input_capacity_mah: u32::MAX,
                input_average_ua: 1,
                expected: u32::MAX,
            },
        ];

        for TimeRemainingTestCase {
            name,
            input_permille,
            input_capacity_mah,
            input_average_ua,
            expected,
        } in test_cases
        {
            let result =
                battery_time_remaining_s(input_permille, input_capacity_mah, input_average_ua);
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
