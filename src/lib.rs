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

        for SoilTestCase { name, input_mv, input_dry_mv, input_wet_mv, expected } in test_cases {
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

        for BatteryTestCase { name, input_mv, input_empty_mv, input_full_mv, expected } in test_cases {
            let result = battery_percent(input_mv, input_empty_mv, input_full_mv);
            assert_eq!(result, expected, "Failed case: '{name}'");
        }
    }
}
