//! The device's behaviour, walked through boots in a scripted world.
//!
//! These are the tests the firmware cannot have on its own: they need a clock
//! that only moves when the code asks, sensors that say what the test wants, and
//! a reboot that is just another loop iteration.

use soil_sensor_core::led::{Color, LedCommand};
use soil_sensor_core::ports::{OperatingMode, Outcome, WakeCause};
use soil_sensor_core::tuning;

use soil_sensor_sim::energy::Energy;
use soil_sensor_sim::{Script, World};

const MINUTE: u64 = 60_000;
const HOUR: u64 = 60 * MINUTE;
const DAY: u64 = 24 * HOUR;

#[test]
fn a_long_idle_boot_samples_once_and_then_sleeps() {
    let mut world = World::new()
        .soil(Script::constant(1200))
        .battery(Script::constant(1400));

    let outcome = world.run_boot(WakeCause::Cold);

    assert_eq!(
        world.soil_reports().len(),
        1,
        "one reading per wake-up is the whole point of the profile"
    );
    assert_eq!(world.battery_reports().len(), 1);

    match outcome {
        Outcome::Sleep { for_ms } => {
            assert!(
                for_ms > u64::from(tuning::LIT_POLL_PERIOD_MS) - 60_000,
                "a long-idle device sleeps for about its idle period, got {for_ms} ms"
            );
        }
        other => panic!("expected a sleep, got {other:?}"),
    }
}

#[test]
fn a_short_idle_device_keeps_sampling_and_does_not_sleep() {
    // Short-idle until the controller registers, one and a half minutes later.
    // The script moves so that every reading is worth reporting: a repeated one
    // is suppressed by the deadband, which would hide the cadence under test.
    let mut world = World::new()
        .soil(Script::new([1200, 1000]))
        .battery(Script::constant(1400))
        .lit_at(90_000);

    world.run_boot(WakeCause::Cold);

    assert!(
        world.soil_reports().len() >= 3,
        "a short-idle device samples on its cadence while it waits to be \
         registered, got {} reports",
        world.soil_reports().len()
    );

    let first_sleep = world.boots().first().expect("one boot").awake_ms;
    assert!(
        first_sleep >= 90_000,
        "it must not sleep before it becomes a long-idle device, woke after {first_sleep} ms"
    );
}

#[test]
fn a_scenario_that_never_becomes_long_idle_is_a_clear_failure_not_a_hang() {
    let result = std::panic::catch_unwind(|| {
        let mut world = World::new().lit_at(u64::MAX).boot_limit(2 * HOUR);
        world.run_boot(WakeCause::Cold);
    });

    assert!(
        result.is_err(),
        "a device that stays reachable by design must not look like a hang"
    );
}

#[test]
fn the_battery_estimate_needs_nothing_from_earlier_boots() {
    // A cold boot, which has no earlier boot to learn from, against a wake-up
    // a day later at the same voltage.
    let mut world = World::new().battery(Script::constant(1400));
    world.run_boot(WakeCause::Cold);
    world.run_until(DAY);

    let estimates = world.battery_estimates();
    let first = estimates.first().expect("an estimate per boot");
    let last = estimates.last().expect("an estimate per boot");
    assert_eq!(world.boots()[0].wake, WakeCause::Cold);
    assert_eq!(
        (first.percent, first.time_remaining_s),
        (last.percent, last.time_remaining_s)
    );
    assert!(first.time_remaining_s.is_some());
}

#[test]
fn the_time_remaining_is_more_pessimistic_than_the_energy_model() {
    let mut world = World::new().battery(Script::constant(1550));
    world.run_until(3 * DAY);

    let energy = Energy::from_world(&world);
    let modelled_s = energy.life_years(soil_sensor_sim::energy::AA_CAPACITY_MAH) * 365.0 * 86_400.0;
    let estimated_s = world
        .battery_estimates()
        .last()
        .and_then(|estimate| estimate.time_remaining_s)
        .expect("every estimate has a time remaining");

    assert!(
        f64::from(estimated_s) < modelled_s,
        "the estimate ({estimated_s} s) must not promise more than the energy model ({modelled_s:.0} s)"
    );
}

#[test]
fn a_boot_after_a_crash_says_so_on_the_leds() {
    let mut world = World::new().after_crash();
    world.run_boot(WakeCause::Timer);
    let after_crash: Vec<LedCommand> = world.leds().iter().map(|(_, cmd)| *cmd).collect();

    world.run_boot(WakeCause::Timer);
    let after_next: Vec<LedCommand> = world.leds().iter().map(|(_, cmd)| *cmd).collect();

    assert_eq!(
        (after_crash, after_next),
        (vec![LedCommand::crashed()], vec![LedCommand::crashed()]),
        "only the boot after the crash blinks"
    );
}

#[test]
fn a_single_press_samples_and_says_so_on_the_leds() {
    // A press is what wakes the device in this profile, so the gesture starts
    // at the boot itself rather than at some later moment the boot would never
    // live to see. The waking press's completion IS the boot's reading: one
    // reading, and the classification of it on the LEDs.
    let mut world = World::new()
        .soil(Script::new([1200, 1000]))
        .battery(Script::constant(1400))
        .press(0, 100);

    world.run_boot(WakeCause::Button);

    assert!(
        world
            .leds()
            .iter()
            .any(|(_, cmd)| matches!(cmd, LedCommand::ClassifyMoisture { .. })),
        "a single press is meant to say how wet the soil is"
    );
    assert_eq!(
        world.soil_reports().len(),
        1,
        "one reading per wake-up: the waking press's, not a second one"
    );
}

#[test]
fn the_boot_does_not_measure_until_the_waking_press_has_spoken() {
    // The waking press might still turn into a triple click or a hold, so
    // nothing is measured until it has resolved - which is also what makes
    // the gesture's sample and the boot's sample the same one.
    let mut world = World::new()
        .soil(Script::new([1200, 1000]))
        .battery(Script::constant(1400))
        .press(1_000, 100);

    world.run_boot(WakeCause::Button);

    let reports = world.soil_reports();
    assert_eq!(reports.len(), 1, "one reading per wake-up");
    assert!(
        reports[0].at_ms >= 1_100 + tuning::BUTTON_TRIPLE_CLICK_WINDOW.as_millis(),
        "the reading waits for the click window to close, taken at {} ms",
        reports[0].at_ms
    );
}

#[test]
fn a_press_keeps_the_boot_awake_while_the_gesture_is_in_progress() {
    // The device would settle into idle at ACTIVE_HOLD and sleep half a second
    // later. This press starts just before that and is still down when the
    // moment arrives.
    let press_at = tuning::ACTIVE_HOLD.as_millis() - 200;
    let mut world = World::new()
        .soil(Script::new([1200, 1000]))
        .press(press_at, 2_000);

    let outcome = world.run_boot(WakeCause::Timer);

    let awake = match outcome {
        Outcome::Sleep { .. } => world.boots().first().expect("one boot").awake_ms,
        other => panic!("expected a sleep eventually, got {other:?}"),
    };
    assert!(
        awake > tuning::ACTIVE_HOLD.as_millis() + tuning::SLEEP_SETTLE.as_millis() + 1_000,
        "a sleep would restart the gesture's timing, so the boot has to wait for \
         it, but it ended after {awake} ms"
    );
    assert!(
        world.soil_reports().len() >= 2,
        "and the gesture it was waiting for still completes"
    );
}

#[test]
fn a_ten_second_hold_asks_for_a_factory_reset() {
    let mut world = World::new().press(1_000, 11_000);

    let outcome = world.run_boot(WakeCause::Button);

    assert_eq!(outcome, Outcome::FactoryReset);
}

#[test]
fn three_presses_start_the_calibration_flow() {
    // The waking gesture resolves before anything is measured, so the script
    // is read in the flow's order: the dry reference, the wet reference, and
    // then the boot's own reading.
    let mut world = World::new()
        .soil(Script::new([1500, 900, 900]))
        // Three taps inside one click window of each other.
        .press(1_000, 100)
        .press(1_300, 100)
        .press(1_600, 100);

    world.run_boot(WakeCause::Button);

    assert_eq!(world.calibration_writes(), 1, "the flow ran exactly once");

    let calibration = world.calibration();
    assert_eq!(
        calibration.dry_mv, 1500,
        "the dry reference is what the probe read during the red half"
    );
    assert_eq!(
        calibration.wet_mv, 900,
        "and the wet reference what it read during the green half"
    );

    let blinks = world
        .leds()
        .iter()
        .filter(|(_, cmd)| matches!(cmd, LedCommand::Blink { .. }))
        .count();
    assert_eq!(blinks, 2, "one cue per half of the flow");

    assert!(
        world.leds().iter().any(|(_, cmd)| matches!(
            cmd,
            LedCommand::Blink {
                color: Color::Red,
                ..
            }
        )),
        "dry is the red half"
    );
    assert!(
        world.leds().iter().any(|(_, cmd)| matches!(
            cmd,
            LedCommand::Blink {
                color: Color::Green,
                ..
            }
        )),
        "wet is the green half"
    );
}

#[test]
fn a_calibration_the_probe_cannot_support_is_rejected() {
    // Dry and wet within the minimum span: the probe was mis-clamped.
    let mut world = World::new()
        .soil(Script::new([1200, 1150]))
        .press(1_000, 100)
        .press(1_300, 100)
        .press(1_600, 100);

    world.run_boot(WakeCause::Button);

    assert_eq!(
        world.calibration_writes(),
        0,
        "a calibration that close is noise, and must not be stored"
    );
    assert_eq!(
        world.calibration(),
        soil_sensor_core::ports::Calibration::DEFAULT
    );
    assert_eq!(
        world.leds().last().map(|(_, cmd)| *cmd),
        Some(LedCommand::calibration_rejected()),
        "the person holding the probe has to learn that it did not take"
    );
}

#[test]
fn the_reboot_count_counts_reboots_but_not_wakes() {
    let mut world = World::new()
        .soil(Script::constant(1200))
        .battery(Script::constant(1400));

    // The ordinary cycle: every boot is a scheduled wake-up, and the spec's
    // RebootCount clause (core spec §11.12.6.2) forbids counting a wake from
    // a low-power or sleep state. None of these count.
    world.run_until(3 * HOUR);

    assert_eq!(
        world.reboot_count_writes(),
        0,
        "a wake from sleep is not a reboot worth counting"
    );

    // Powering on is a reboot, and counts - once per boot, however long the
    // wake cycle around it runs.
    world.run_boot(WakeCause::Cold);
    world.run_until(world.now_ms() + 3 * HOUR);

    assert_eq!(
        world.reboot_count_writes() as usize,
        world
            .boots()
            .iter()
            .filter(|boot| boot.wake == WakeCause::Cold)
            .count(),
        "every boot that is not a wake from sleep wrote the count once"
    );
}

#[test]
fn a_warm_boot_never_counts_a_reboot() {
    // Short-idle for the first ninety seconds, so the boot stays awake well
    // past the point a cold boot would have been counted - and still does not
    // count, because staying awake is not a fault either.
    let mut world = World::new()
        .soil(Script::new([1200, 1000]))
        .battery(Script::constant(1400))
        .lit_at(90_000);

    world.run_boot(WakeCause::Timer);

    assert_eq!(
        world.reboot_count_writes(),
        0,
        "a boot that overstayed the deadline without sleeping is still not a fault"
    );
}

#[test]
fn the_active_window_dominates_the_energy_budget() {
    let mut world = World::new().battery(Script::ramp(1500, 1400, 400));

    world.run_until(7 * DAY);

    let energy = Energy::from_world(&world);

    // The point of the long-idle profile: almost all of the time is spent
    // asleep, and the device lasts years rather than days on a cell.
    assert!(
        energy.awake_fraction() < 0.05,
        "awake {:.3}% of the time",
        energy.awake_fraction() * 100.0
    );
    assert!(
        energy.life_years(2500.0) > 0.5,
        "a model, but it should at least be in the right order of magnitude"
    );
}

#[test]
fn the_first_boot_of_a_simulation_is_a_cold_one() {
    let mut world = World::new().lit_at(90_000);

    let first = world.run_boot(WakeCause::Cold);
    assert!(matches!(first, Outcome::Sleep { .. }));

    assert_eq!(
        world.boots().first().expect("one boot").wake,
        WakeCause::Cold
    );
}

#[test]
fn boots_after_the_first_are_woken_by_the_sleep_timer() {
    let mut world = World::new();

    world.run_until(2 * HOUR);

    let wakes: Vec<WakeCause> = world.boots().iter().skip(1).map(|b| b.wake).collect();
    assert!(!wakes.is_empty());
    assert!(
        wakes.iter().all(|wake| *wake == WakeCause::Timer),
        "with no button in the script, every wake is the sleep timer"
    );
}

#[test]
fn the_device_operates_as_a_short_idle_one_until_a_controller_registers() {
    let mut world = World::new().lit_at(90_000);

    // Before the switch, the profile is the short-idle one.
    assert_eq!(
        world.icd_mode_now(),
        OperatingMode::Sit,
        "nothing has registered yet"
    );

    world.run_boot(WakeCause::Cold);

    assert_eq!(
        world.icd_mode_now(),
        OperatingMode::Lit,
        "and by the end of the boot the controller has registered"
    );
}

#[test]
fn a_calibration_survives_a_reboot() {
    // The probe's own range, captured by the flow during the first boot: dry in
    // air, wet in water. Both sit well above the firmware's built-in dry
    // reference, which is the point of calibrating at all. The waking gesture
    // resolves first, so the flow's two captures open the script, and the
    // boot's own reading follows them.
    let mut world = World::new()
        .soil(Script::new([2500, 1200, 1200]))
        .press(1_000, 100)
        .press(1_300, 100)
        .press(1_600, 100);

    world.run_boot(WakeCause::Button);
    assert_eq!(
        world.calibration_writes(),
        1,
        "the flow stored a calibration"
    );
    assert_eq!(world.calibration().dry_mv, 2500);
    assert_eq!(world.calibration().wet_mv, 1200);

    // A later boot, with the probe in soil reading 1200 mV. Against the
    // calibration it captured, that is bone dry to fully wet; against the
    // firmware's defaults it is a little over half. The two answers are far
    // enough apart to tell which one the device used.
    world.set_soil(Script::constant(1200));
    let before = world.soil_reports().len();
    world.run_boot(WakeCause::Timer);

    let reported = world.soil_reports()[before].percent;
    assert!(
        reported >= 90,
        "the stored calibration has to be loaded on the next boot: 1200 mV \
         against dry=2500/wet=1200 is fully wet, but it came out as {reported}%, \
         which is what the built-in defaults give",
    );
}
