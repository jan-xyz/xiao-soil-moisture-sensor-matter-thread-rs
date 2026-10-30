//! One boot of the device, from power-up to whatever ends it.
//!
//! A deep sleep is a reboot, so a boot is the natural unit of behaviour: this
//! module drives exactly one, and reports how it ended. The composition root
//! performs the ending - powering the chip down, or erasing and restarting -
//! because both are irreversible. The simulator performs it too, by starting
//! another boot and carrying forward only the state whose lifetime says it
//! should survive.
//!
//! The loop is deliberately dull:
//!
//!   1. if a gesture asked for a calibration, run it to completion;
//!   2. if a sample is due, take one and report it;
//!   3. on a boot that is not a wake from sleep, if the reboot count is due,
//!      store it;
//!   4. if the world allows it, end the boot by sleeping;
//!   5. otherwise wait for the deadline, a button edge, or a protocol change.
//!
//! Every decision in it is a value, and every value is in [`tuning`], so the
//! whole thing can be walked through with a scripted world.

use log::{info, warn};

use crate::button::{ButtonEvent, ButtonFsm};
use crate::calibration::{self, CalibrationOutcome};
use crate::led::LedCommand;
use crate::math::{battery_permille, battery_time_remaining_s, soil_moisture_percent};
use crate::ports::{
    BatteryReading, Event, IcdState, MatterPorts, OperatingMode, Outcome, Ports, PowerMode,
    WakeCause,
};
use crate::time::Instant;
use crate::tuning;

/// What a boot needs to know about itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Boot {
    pub wake: WakeCause,
    pub now: Instant,
    /// The boot before this one ended without a planned end: a panic, a
    /// watchdog reset or a brown-out.
    pub after_crash: bool,
}

/// The state of one boot. Volatile by definition: a new boot starts from
/// [`App::new`] and nothing else.
pub struct App {
    calibration: crate::ports::Calibration,
    button: ButtonFsm,
    next_sample: Instant,
    /// Set when a gesture asked for a sample that should also be shown on the
    /// status LEDs.
    sample_with_led: bool,
    /// Set when a gesture asked for the calibration flow.
    calibration_requested: bool,
    reboot_count_due: Instant,
    reboot_count_persisted: bool,
    /// When the protocol stack last reported idle mode, and `None` while it is
    /// active. The settle delay before sleeping is measured from here.
    idle_since: Option<Instant>,
}

impl App {
    /// The calibration is a parameter rather than a default on purpose: a boot
    /// that measured against the firmware's built-in references instead of the
    /// ones a person captured is wrong in a way that looks like a sensor fault,
    /// and that is exactly the bug this signature prevents.
    pub fn new(boot: Boot, calibration: crate::ports::Calibration) -> Self {
        Self {
            calibration,
            button: ButtonFsm::new(boot.wake == WakeCause::Button, boot.now),
            next_sample: boot.now,
            sample_with_led: false,
            calibration_requested: false,
            // The reboot count is a best-effort counter of reboots, and the
            // spec is explicit about the one case it must not count (core
            // spec §11.12.6.2): "The RebootCount attribute SHALL NOT be
            // incremented when a Node wakes from a low-power or sleep state."
            // A deep-sleep wake is this device's ordinary cycle, so warm boots
            // neither advance nor write the count; powering on, a panic or a
            // watchdog reset all count. The value a boot serves is one past
            // the persisted one - the same value the next counted boot will
            // write - so nothing is lost by not persisting elsewhere.
            reboot_count_persisted: boot.wake != WakeCause::Cold,
            reboot_count_due: boot.now.saturating_add(tuning::REBOOT_COUNT_HEALTHY_AFTER),
            idle_since: None,
        }
    }

    /// The references in effect.
    pub fn calibration(&self) -> crate::ports::Calibration {
        self.calibration
    }

    /// Do whatever is due now, and end the boot if it is over.
    ///
    /// Returns `None` when there is nothing left to do but wait.
    ///
    /// This is the operational phase's step, and it speaks through the
    /// protocol stack - the reboot count's two write sites both live here,
    /// which is why it takes [`MatterPorts`]. The wake phase has its own,
    /// smaller step in [`Self::step_wake`].
    pub async fn step<P: MatterPorts>(&mut self, ports: &mut P) -> Option<Outcome> {
        self.step_wake(ports).await;

        // The reboot count, once per counted boot - every boot except a wake
        // from sleep, per the spec's RebootCount clause - and never before
        // that boot has proved it is not a boot loop.
        if !self.reboot_count_persisted && ports.now() >= self.reboot_count_due {
            self.persist_reboot_count(ports);
            return None;
        }

        // Anything else the world is asking for?
        let now = ports.now();
        let icd = ports.icd();
        self.track_idle(icd, now);

        let for_ms = self.sleep_for(ports, icd)?;

        // Nothing may be missed between deciding to sleep and sleeping: an edge
        // that arrived while this boot was busy would otherwise be read as an
        // idle device.
        self.drain_button(ports);

        if self.button.is_active() {
            return None;
        }

        // A boot that got this far is healthy by definition, so this is also
        // where the reboot count is written - on a counted boot. `App::new`
        // marks wake-from-sleep boots as already done, so a scheduled wake-up
        // leaves the persisted count untouched.
        if !self.reboot_count_persisted {
            self.persist_reboot_count(ports);
        }

        info!("ICD idle: deep-sleeping for {for_ms} ms");
        Some(Outcome::Sleep { for_ms })
    }

    /// Everything that is due before the protocol stack exists: the
    /// calibration a waking gesture asked for, and the boot's one reading.
    ///
    /// Doing one thing and returning would be wrong here: taking a reading
    /// advances the clock, and the operational loop re-checks its sleep
    /// decision against the advanced clock immediately. The same re-checking
    /// is what stops a boot from sampling forever instead of noticing it may
    /// sleep.
    async fn step_wake<P: Ports>(&mut self, ports: &mut P) {
        loop {
            let now = ports.now();

            // A calibration owns the boot until it finishes. The protocol
            // stack is kept active across it by the gesture that started it.
            if self.calibration_requested {
                self.calibration_requested = false;
                if let CalibrationOutcome::Accepted(calibration) = calibration::run(ports).await {
                    self.calibration = calibration;
                }
                continue;
            }

            // One reading per due sample. This is the whole reason a boot
            // exists in the long-idle profile, so it must happen before
            // anything can end the boot.
            if now >= self.next_sample {
                let with_led = core::mem::take(&mut self.sample_with_led);
                self.sample(ports, with_led).await;
                self.next_sample = ports.now().saturating_add(tuning::SAMPLE_PERIOD);
                continue;
            }

            break;
        }
    }

    /// Handle whatever the world delivered, and end the boot if it is over.
    pub fn on_event<P: Ports>(&mut self, event: Event, ports: &mut P) -> Option<Outcome> {
        let now = ports.now();

        let gesture = match event {
            Event::Button(edge) => self.button.on_edge(edge, now),
            Event::Deadline => self.button.on_deadline(now),
            // The protocol stack changed its mind; the next step re-reads it.
            Event::Icd => None,
        };

        let gesture = gesture?;

        match gesture {
            ButtonEvent::SampleNow => {
                // A local press is also a user active-mode trigger: it reopens
                // the active window so the resulting report, and any reply to
                // it, go out promptly.
                ports.request_active(None);
                self.sample_with_led = true;
                self.next_sample = now;
                None
            }
            ButtonEvent::Calibrate => {
                ports.request_active(Some(tuning::CALIBRATION_ACTIVE_HOLD));
                self.calibration_requested = true;
                None
            }
            ButtonEvent::FactoryReset => Some(Outcome::FactoryReset),
        }
    }

    /// When this boot next has something to do.
    ///
    /// Always strictly after `now`: a deadline that has already passed is one
    /// this boot has finished with, and returning it would ask the caller to
    /// wait for an instant that has gone, which is a wait nothing can end.
    pub fn next_deadline(&self, now: Instant) -> Instant {
        let mut earliest = self.next_sample;

        let consider = |candidate: Option<Instant>, earliest: &mut Instant| {
            if let Some(candidate) = candidate {
                if candidate > now && candidate < *earliest {
                    *earliest = candidate;
                }
            }
        };

        consider(
            (!self.reboot_count_persisted).then_some(self.reboot_count_due),
            &mut earliest,
        );
        consider(self.button.next_deadline(), &mut earliest);
        consider(
            self.idle_since
                .map(|idle_since| idle_since.saturating_add(tuning::SLEEP_SETTLE)),
            &mut earliest,
        );

        earliest
    }

    /// Whether the press that woke the device is still mid-gesture, and the
    /// boot's reading has to wait for it.
    pub(crate) fn waking_press_pending(&self) -> bool {
        self.button.waking_press_pending()
    }

    /// The deadline the waking press is waiting for. Only the button's own
    /// timing is relevant here: every other deadline of the boot sits behind
    /// the reading that this press is holding off, and none of them may fire
    /// while the gesture is still undecided.
    fn wake_deadline(&self) -> Instant {
        self.button
            .next_deadline()
            .expect("a waking press always waits for something")
    }

    /// Take one reading of both sensors and report them.
    async fn sample<P: Ports>(&mut self, ports: &mut P, with_led: bool) {
        let soil_mv = ports.read_soil_mv().await;
        let percent = soil_moisture_percent(
            soil_mv as i32,
            self.calibration.dry_mv,
            self.calibration.wet_mv,
        );
        ports.report_soil(percent);
        if with_led {
            ports.send_led(LedCommand::classify(percent));
        }

        let battery_mv = ports.read_battery_mv().await;
        let reading = estimate_battery(battery_mv);
        if reading.is_none() {
            info!("Battery: {battery_mv} mV is no cell, so the charge is unknown");
        }
        ports.report_battery(reading);
    }

    fn persist_reboot_count<P: MatterPorts>(&mut self, ports: &mut P) {
        ports.persist_reboot_count();
        self.reboot_count_persisted = true;
    }

    fn track_idle(&mut self, icd: IcdState, now: Instant) {
        if icd.power_mode == PowerMode::Idle {
            self.idle_since.get_or_insert(now);
        } else {
            self.idle_since = None;
        }
    }

    /// How long this boot may sleep for, if it may sleep at all.
    ///
    /// Every condition is a reason the device is still needed.
    fn sleep_for<P: Ports>(&self, ports: &P, icd: IcdState) -> Option<u64> {
        // Until a peer registers for check-ins the device operates as a short
        // idle device, which has to stay reachable within a few seconds. A
        // sleep that long, plus the reboot it ends in, would break that.
        if icd.operating_mode != OperatingMode::Lit {
            return None;
        }

        if icd.power_mode != PowerMode::Idle {
            return None;
        }

        if self.button.is_active() {
            return None;
        }

        // Let whatever the active period changed reach flash first.
        let idle_since = self.idle_since?;
        if ports.now().saturating_since(idle_since) < tuning::SLEEP_SETTLE {
            return None;
        }

        // Sleep until the next poll is due, and never longer than the idle
        // period: a long-idle device polls once per idle period, and the poll
        // is what keeps it in its parent's child table.
        let poll_ms = u64::from(icd.poll_interval_ms);
        let idle_ms = icd
            .idle_until
            .map(|until| until.saturating_since(ports.now()).as_millis())
            .unwrap_or(poll_ms);

        Some(poll_ms.min(idle_ms).max(tuning::SLEEP_MIN.as_millis()))
    }

    fn drain_button<P: Ports>(&mut self, ports: &mut P) {
        while let Some(edge) = ports.try_next_button_edge() {
            let now = ports.now();
            match self.button.on_edge(edge, now) {
                Some(ButtonEvent::FactoryReset) => {
                    // Nothing to do here: the caller re-checks `is_active` and
                    // the reset is asked for again on the next deadline.
                    warn!("Factory reset requested while preparing to sleep");
                }
                Some(_) => {
                    // A gesture completed while this boot was busy. It is not
                    // worth acting on now - the boot is about to end - but it
                    // must keep the boot awake.
                }
                None => {}
            }
        }
    }
}

/// The estimate from one reading. Nothing about the battery is kept between
/// boots, so no stored state can ever be wrong.
fn estimate_battery(rest_mv: u32) -> Option<BatteryReading> {
    if rest_mv < tuning::BATTERY_PRESENT_MIN_MV {
        return None;
    }

    let permille = battery_permille(rest_mv as i32, &tuning::BATTERY_CURVE);
    let time_remaining_s = battery_time_remaining_s(
        permille,
        tuning::BATTERY_CAPACITY_MAH,
        tuning::BATTERY_AVERAGE_UA,
    );

    // Rounded down, so the reported charge is never more than the curve's.
    Some(BatteryReading {
        percent: (permille / 10) as u8,
        rest_mv,
        time_remaining_s: Some(time_remaining_s),
    })
}

/// The wake phase of a boot: the press that woke the device resolved, the
/// boot's one reading taken.
///
/// This phase runs before the protocol stack exists, which is the whole point:
/// the press that wakes the device is answered without waiting for the stack's
/// seconds of startup. What it needs from the world is [`Ports`] - a port set
/// of this phase cannot reach the stack at all.
///
/// Returns the [`App`] the operational phase continues from, or the outcome
/// that ends the boot before the stack ever matters - a factory reset,
/// requested by holding the button through the wake phase.
pub async fn run_wake<P: Ports>(ports: &mut P, boot: Boot) -> Result<App, Outcome> {
    // Nothing else shows a crash: the serial console is gone after the first
    // deep sleep, and the controller only sees a device that went quiet.
    if boot.after_crash {
        ports.send_led(LedCommand::crashed());
    }

    // A press that woke the device is still in progress, so the boot window has
    // to be wide enough for the gesture it is part of to finish.
    if boot.wake == WakeCause::Button {
        ports.request_active(Some(tuning::BUTTON_WAKE_HOLD));
    }

    // The stored calibration has to be in hand before anything is measured with
    // it, and the first thing a boot does is take a reading.
    let calibration = ports.load_calibration().await;
    let mut app = App::new(boot, calibration);

    // The waking press speaks before the boot measures. A single press wants
    // the reading shown, a triple click wants the calibration flow, and a
    // long hold wants the fabric gone - and since the reading is the single
    // press's answer, nothing is measured until the press has said which of
    // the three it was. That ordering is also what keeps the boot to its one
    // reading: the gesture's completion arms the very sample the phase is
    // about to take, instead of adding a second one.
    while app.waking_press_pending() {
        let event = ports.next_event(app.wake_deadline()).await;

        if let Some(outcome) = app.on_event(event, ports) {
            return Err(outcome);
        }
    }

    // The reading, and the calibration the gesture may have asked for. There
    // is no sleep decision here: the protocol stack has not spoken yet, so
    // there is nothing to sleep from.
    app.step_wake(ports).await;

    Ok(app)
}

/// The operational phase of a boot: everything the protocol stack gates.
///
/// Continues from the [`App`] the wake phase returned, and needs
/// [`MatterPorts`] - the reboot count speaks through the stack's storage. The
/// protocol stack itself is expected to be starting up or running alongside;
/// ending the boot in a deep sleep is this phase's job.
pub async fn run_boot<P: MatterPorts>(ports: &mut P, mut app: App) -> Outcome {
    loop {
        if let Some(outcome) = app.step(ports).await {
            return outcome;
        }

        let event = ports.next_event(app.next_deadline(ports.now())).await;

        if let Some(outcome) = app.on_event(event, ports) {
            return outcome;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::{ButtonEdge, Calibration, IcdState};
    use crate::time::Millis;

    /// A world that does nothing of its own accord. The test decides what time
    /// it is and what the protocol stack thinks; everything else is a no-op.
    ///
    /// This is enough to exercise [`App::sleep_for`], which is the one decision
    /// in this module that is worth pinning down here. The behaviour of the
    /// whole loop is covered by the simulation, which runs it for real.
    struct Fake {
        now: Instant,
        icd: IcdState,
    }

    impl Fake {
        fn new(icd: IcdState) -> Self {
            Self {
                now: Instant::ZERO,
                icd,
            }
        }

        fn at(mut self, now: Instant) -> Self {
            self.now = now;
            self
        }
    }

    impl Ports for Fake {
        fn now(&self) -> Instant {
            self.now
        }
        async fn delay(&mut self, d: Millis) {
            self.now = self.now + d;
        }
        async fn next_event(&mut self, _deadline: Instant) -> Event {
            Event::Deadline
        }
        fn try_next_button_edge(&mut self) -> Option<ButtonEdge> {
            None
        }
        fn send_led(&mut self, _cmd: LedCommand) {}
        async fn read_soil_mv(&mut self) -> u32 {
            1200
        }
        async fn read_battery_mv(&mut self) -> u32 {
            1400
        }
        fn icd(&self) -> IcdState {
            self.icd
        }
        fn request_active(&mut self, _hold: Option<Millis>) {}
        fn report_soil(&mut self, _percent: u8) {}
        fn report_battery(&mut self, _reading: Option<BatteryReading>) {}
        async fn load_calibration(&mut self) -> Calibration {
            Calibration::DEFAULT
        }
        async fn store_calibration(&mut self, _c: Calibration) -> bool {
            true
        }
    }

    fn lit_active() -> IcdState {
        IcdState {
            power_mode: PowerMode::Active,
            operating_mode: OperatingMode::Lit,
            poll_interval_ms: tuning::LIT_POLL_PERIOD_MS,
            idle_until: None,
        }
    }

    fn lit_idle(now: Instant) -> IcdState {
        IcdState {
            power_mode: PowerMode::Idle,
            operating_mode: OperatingMode::Lit,
            poll_interval_ms: tuning::LIT_POLL_PERIOD_MS,
            idle_until: Some(now + Millis::from_millis(u64::from(tuning::LIT_POLL_PERIOD_MS))),
        }
    }

    /// An app that has been idle long enough to be allowed to sleep, and a
    /// world that agrees.
    fn settled(icd: IcdState) -> (App, Fake) {
        let now = Instant::from_secs(60);
        let mut app = App::new(
            Boot {
                wake: WakeCause::Timer,
                now: Instant::ZERO,
                after_crash: false,
            },
            Calibration::DEFAULT,
        );
        app.idle_since = Some(now.saturating_sub(tuning::SLEEP_SETTLE));
        (app, Fake::new(icd).at(now))
    }

    #[test]
    fn a_short_idle_device_never_sleeps() {
        let (app, ports) = settled(lit_idle(Instant::from_secs(60)));
        let mut icd = ports.icd();
        icd.operating_mode = OperatingMode::Sit;

        assert_eq!(
            app.sleep_for(&ports, icd),
            None,
            "a short idle device has to stay reachable within seconds"
        );
    }

    #[test]
    fn an_active_device_never_sleeps() {
        let (app, ports) = settled(lit_active());
        assert_eq!(app.sleep_for(&ports, ports.icd()), None);
    }

    #[test]
    fn a_device_that_has_not_settled_yet_never_sleeps() {
        let now = Instant::from_secs(60);
        let icd = lit_idle(now);
        let ports = Fake::new(icd).at(now);

        let mut app = App::new(
            Boot {
                wake: WakeCause::Timer,
                now: Instant::ZERO,
                after_crash: false,
            },
            Calibration::DEFAULT,
        );
        app.idle_since = Some(now);

        assert_eq!(
            app.sleep_for(&ports, icd),
            None,
            "the settle delay lets flash writes finish"
        );

        app.idle_since = Some(now.saturating_sub(tuning::SLEEP_SETTLE));
        assert!(app.sleep_for(&ports, icd).is_some());
    }

    #[test]
    fn a_gesture_in_progress_keeps_the_boot_awake() {
        let (app, ports) = settled(lit_idle(Instant::from_secs(60)));
        let _ = app;

        // A boot caused by the button starts mid-press.
        let mut pressed = App::new(
            Boot {
                wake: WakeCause::Button,
                now: Instant::from_secs(60),
                after_crash: false,
            },
            Calibration::DEFAULT,
        );
        pressed.idle_since = Some(Instant::from_secs(60).saturating_sub(tuning::SLEEP_SETTLE));

        assert_eq!(
            pressed.sleep_for(&ports, ports.icd()),
            None,
            "the press that woke the device is still going on"
        );
    }

    #[test]
    fn the_sleep_length_is_the_poll_interval_capped_by_the_idle_period() {
        // A poll interval far longer than the remaining idle period: the idle
        // period wins, because that is when the device has to poll again.
        let now = Instant::from_secs(60);
        let (app, _) = settled(lit_idle(now));
        let mut icd = lit_idle(now);
        icd.poll_interval_ms = 10_000_000;
        let ports = Fake::new(icd).at(now);
        assert_eq!(
            app.sleep_for(&ports, icd),
            Some(u64::from(tuning::LIT_POLL_PERIOD_MS))
        );

        // The other way round: the poll interval wins.
        let mut icd = lit_idle(now);
        icd.poll_interval_ms = 5_000;
        let ports = Fake::new(icd).at(now);
        assert_eq!(app.sleep_for(&ports, icd), Some(5_000));
    }

    #[test]
    fn the_sleep_floor_keeps_a_wake_up_worth_it() {
        let now = Instant::from_secs(60);
        let (app, _) = settled(lit_idle(now));

        let mut icd = lit_idle(now);
        icd.poll_interval_ms = 1;
        icd.idle_until = Some(now + Millis::from_millis(1));
        let ports = Fake::new(icd).at(now);

        assert_eq!(
            app.sleep_for(&ports, icd),
            Some(tuning::SLEEP_MIN.as_millis()),
            "the reboot a wake-up costs is worth more than a very short sleep"
        );
    }

    struct EstimateTestCase {
        name: &'static str,
        input_mv: u32,
        expected: Option<BatteryReading>,
    }

    #[test]
    fn test_estimate_battery() {
        let test_cases = vec![
            EstimateTestCase {
                name: "an empty holder on USB is no cell",
                input_mv: 375,
                expected: None,
            },
            EstimateTestCase {
                name: "just below the threshold is no cell",
                input_mv: tuning::BATTERY_PRESENT_MIN_MV - 1,
                expected: None,
            },
            EstimateTestCase {
                name: "at the threshold is a cell, and an empty one",
                input_mv: tuning::BATTERY_PRESENT_MIN_MV,
                expected: Some(BatteryReading {
                    percent: 0,
                    rest_mv: tuning::BATTERY_PRESENT_MIN_MV,
                    time_remaining_s: Some(0),
                }),
            },
        ];

        for EstimateTestCase {
            name,
            input_mv,
            expected,
        } in test_cases
        {
            let result = estimate_battery(input_mv);
            assert_eq!(result, expected, "Failed case: '{name}'");
        }
    }
}
