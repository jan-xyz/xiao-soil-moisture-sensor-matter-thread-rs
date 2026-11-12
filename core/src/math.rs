//! The plain arithmetic.
//!
//! Nothing here reads a clock, a pin or a byte of flash, so all of it is
//! exercised on the host. That is the pattern the rest of the decision layer
//! follows: if a calculation can be written without the world, it is.

use crate::tuning;

/// Piecewise-linear interpolation between the wet (low mV) and dry (high mV)
/// soil-probe calibration references, clamped to 0-100%.
///
/// The bend sits at [`tuning::SOIL_CAL_BREAK_FRAC_PERMILLE`] of the span from
/// the wet reference and maps to [`tuning::SOIL_CAL_BREAK_PERCENT`] percent.
/// It is derived from the same two references, so the calibration flow still
/// captures only dry and wet. The placement near the dry end mirrors the
/// segmented calibration of Szypłowska et al. 2021 (Geoderma 401, 115288):
/// their breakpoint lies at a permittivity much closer to air than to water,
/// with the dry side of the curve steeper than the wet side.
///
/// Because the bend is strictly inside the span, both segments slope down in
/// mV, so the percentage can only fall as the soil dries - it cannot rise in
/// any region. If the constants were ever misconfigured so the bend left the
/// span, the plain linear map is used instead, which is still monotone for
/// any valid span.
#[must_use]
pub fn soil_moisture_percent(mv: i32, dry_mv: i32, wet_mv: i32) -> u8 {
    let span = dry_mv - wet_mv;
    if span <= 0 {
        return 0;
    }
    let break_mv = wet_mv + span * tuning::SOIL_CAL_BREAK_FRAC_PERMILLE / 1000;
    let break_pct = i32::from(tuning::SOIL_CAL_BREAK_PERCENT);
    let pct = if break_mv > wet_mv && break_mv < dry_mv {
        if mv >= break_mv {
            break_pct * (dry_mv - mv) / (dry_mv - break_mv)
        } else {
            100 - (100 - break_pct) * (mv - wet_mv) / (break_mv - wet_mv)
        }
    } else {
        100 * (dry_mv - mv) / span
    };
    u8::try_from(pct.clamp(0, 100)).unwrap_or(0)
}

/// The charge left in a cell at resting voltage `mv`, in permille, read off
/// `curve`: (millivolts, permille) points in rising order, joined by straight
/// lines and clamped at both ends.
#[must_use]
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
        .map_or(highest, |pair| {
            let ((from_mv, from), (to_mv, to)) = (pair[0], pair[1]);
            let span_mv = i64::from(to_mv) - i64::from(from_mv);
            if span_mv <= 0 {
                return from;
            }
            let rise = i64::from(to) - i64::from(from);
            let offset = i64::from(from) + rise * (i64::from(mv) - i64::from(from_mv)) / span_mv;
            u32::try_from(offset.clamp(0, i64::from(u32::MAX))).unwrap_or(u32::MAX)
        })
}

/// Whether a new reading moved far enough from the last *reported* one to be
/// reported. Comparing against the last report rather than the previous sample
/// lets a slow trend accumulate until it crosses `min_delta`, while jitter
/// below `min_delta` still does not wake the radio. The first reading always
/// reports.
#[must_use]
pub fn should_report(last_reported: Option<u32>, current: u32, min_delta: u32) -> bool {
    match last_reported {
        Some(last_reported) => last_reported.abs_diff(current) >= min_delta,
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tuning;

    struct SoilTestCase {
        name: &'static str,
        input_mv: i32,
        input_dry_mv: i32,
        input_wet_mv: i32,
        expected: u8,
    }

    #[test]
    fn test_soil_moisture_percent() {
        // With dry=1600/wet=900 the derived bend sits at 900 + 700*850/1000 =
        // 1495 mV, mapping to SOIL_CAL_BREAK_PERCENT (30%): the dry segment
        // spans 105 mV for 30%, the wet segment 595 mV for 70%.
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
                name: "at the derived bend reads the bend percent",
                input_mv: 1495,
                input_dry_mv: 1600,
                input_wet_mv: 900,
                expected: 30,
            },
            SoilTestCase {
                name: "one mV below the bend reads one step wetter",
                input_mv: 1494,
                input_dry_mv: 1600,
                input_wet_mv: 900,
                expected: 31,
            },
            SoilTestCase {
                name: "one mV above the bend reads one step drier",
                input_mv: 1496,
                input_dry_mv: 1600,
                input_wet_mv: 900,
                expected: 29,
            },
            SoilTestCase {
                name: "mid-span reads above the linear 50%, the wet side being shallower",
                input_mv: 1250,
                input_dry_mv: 1600,
                input_wet_mv: 900,
                expected: 59,
            },
            SoilTestCase {
                name: "near dry the curve is steeper than linear: 50 mV off dry is 14%, not 7%",
                input_mv: 1550,
                input_dry_mv: 1600,
                input_wet_mv: 900,
                expected: 14,
            },
            SoilTestCase {
                name: "the bend follows the calibrated span, not a fixed mV",
                input_mv: 2305,
                input_dry_mv: 2500,
                input_wet_mv: 1200,
                expected: 30,
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

    #[test]
    fn test_soil_moisture_percent_never_rises_as_mv_rises() {
        // Drying raises mV, so the reading may only fall as mV rises - in
        // every region, across the bend and past both references.
        let (dry, wet) = (1600, 900);
        let mut previous = u8::MAX;
        for mv in (wet - 500)..=(dry + 500) {
            let pct = soil_moisture_percent(mv, dry, wet);
            assert!(
                pct <= previous,
                "at {mv} mV the reading rose to {pct}% (previous {previous}%)"
            );
            previous = pct;
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
            BatteryTestCase {
                name: "a fresh lithium cell reads full",
                input_mv: 1800,
                input_curve: &tuning::BATTERY_CURVE,
                expected: 1000,
            },
            BatteryTestCase {
                name: "a fresh alkaline cell reads full",
                input_mv: 1600,
                input_curve: &tuning::BATTERY_CURVE,
                expected: 1000,
            },
            BatteryTestCase {
                name: "a rested full NiMH cell reads full",
                input_mv: 1350,
                input_curve: &tuning::BATTERY_CURVE,
                expected: 1000,
            },
            BatteryTestCase {
                name: "the end of the NiMH plateau is already a warning",
                input_mv: 1200,
                input_curve: &tuning::BATTERY_CURVE,
                expected: 100,
            },
            BatteryTestCase {
                name: "an alkaline cell with a little left is already critical",
                input_mv: 1150,
                input_curve: &tuning::BATTERY_CURVE,
                expected: 30,
            },
            BatteryTestCase {
                name: "below the knee of every chemistry reads empty",
                input_mv: 1050,
                input_curve: &tuning::BATTERY_CURVE,
                expected: 0,
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
