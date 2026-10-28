//! A scripted world, and the ports that connect it to the boot loop.
//!
//! The world owns everything a boot cannot: the clock, the durable store, the
//! retained store, and the script of what the sensors and the user do. A boot is
//! then a function of that state - which is why running thirty simulated days is
//! just a loop, and why the answer is deterministic.
//!
//! It is deliberately *not* a model of the silicon. It cannot tell you whether a
//! power domain powers down, and it does not model radio timing. What it can
//! tell you is what the device decides, and what that costs.

use std::collections::VecDeque;

use soil_sensor_core::led::LedCommand;
use soil_sensor_core::ports::{
    ButtonEdge, Calibration, Event, IcdState, MatterPorts, OperatingMode, Outcome, Ports,
    PowerMode, Retained, WakeCause,
};
use soil_sensor_core::time::{Instant as CoreInstant, Millis};
use soil_sensor_core::tuning;

use rs_matter::dm::Dataver;

use soil_sensor_node::power_source::ClusterHandler as _;
use soil_sensor_node::soil_measurement::ClusterHandler as _;
use soil_sensor_node::{BatteryCell, PowerSourceHandler, SoilMeasurementHandler, SoilMoistureCell};

/// A repeating list of sensor readings.
#[derive(Clone, Debug)]
pub struct Script {
    values: Vec<u32>,
    index: usize,
}

impl Script {
    pub fn new(values: impl IntoIterator<Item = u32>) -> Self {
        let values: Vec<u32> = values.into_iter().collect();
        assert!(!values.is_empty(), "a script needs at least one reading");
        Self { values, index: 0 }
    }

    pub fn constant(value: u32) -> Self {
        Self::new([value])
    }

    /// A reading that walks from `from` to `to` over `steps` reads, then stays.
    pub fn ramp(from: u32, to: u32, steps: u32) -> Self {
        let steps = steps.max(1);
        let values = (0..=steps).map(|i| {
            let span = i64::from(to) - i64::from(from);
            (i64::from(from) + span * i64::from(i) / i64::from(steps)) as u32
        });
        Self::new(values)
    }

    fn next(&mut self) -> u32 {
        let value = self.values[self.index];
        self.index = (self.index + 1) % self.values.len();
        value
    }
}

/// What the protocol stack thinks, as the world models it.
///
/// A real stack goes active after boot and after every message, and idle
/// otherwise. The simulation reproduces that shape with a deadline, and lets the
/// script choose the operating mode - which is what a controller registering for
/// check-ins would change.
#[derive(Clone, Copy, Debug)]
struct IcdModel {
    /// Absolute virtual time from which the device operates as a long-idle
    /// device. Before it, it operates as a short-idle one, which has to stay
    /// reachable within seconds and therefore may not sleep at all.
    lit_at: u64,
    /// Absolute virtual time the active window ends.
    active_until: u64,
    poll_interval_ms: u32,
}

/// One boot, as the world saw it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BootRecord {
    pub started_at_ms: u64,
    pub wake: WakeCause,
    pub outcome: Outcome,
    /// How long the boot stayed awake, in milliseconds.
    pub awake_ms: u64,
}

/// A report the decision layer asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Report {
    pub at_ms: u64,
    pub percent: u8,
    /// The protocol stack's dataver after the call. It only moves when the
    /// reading was worth reporting, so this is where the deadband shows up.
    pub dataver: u32,
}

pub struct World {
    /// Virtual milliseconds since the simulation began.
    clock: u64,
    /// Virtual milliseconds at which the current boot began.
    boot_at: u64,

    soil: Script,
    battery: Script,

    /// Button edges, as (absolute virtual time, edge).
    button: VecDeque<(u64, ButtonEdge)>,

    icd: IcdModel,
    /// The protocol stack's power mode, as last reported to the boot.
    icd_active: bool,
    /// The operating mode, as last reported to the boot.
    operating_mode_lit: bool,
    /// Whether that mode has changed since the boot was last told. The real
    /// stack notifies through a latching `Notification`, so a change that
    /// happens while the boot is busy is still delivered on its next wait -
    /// and a simulation that dropped it would be testing a different device.
    icd_changed: bool,

    /// The durable store. Survives everything, including the removal of power -
    /// the simulation never removes power, so it is simply never cleared.
    calibration: Calibration,
    /// How many times the calibration has been written.
    calibration_writes: u32,

    /// The retained store. Survives a reboot; `World::power_cycle` clears it.
    retained: Retained,

    leds: Vec<(u64, LedCommand)>,
    soil_reports: Vec<Report>,
    battery_reports: Vec<Report>,
    reboot_count_writes: u32,
    active_requests: Vec<(u64, Option<Millis>)>,

    boots: Vec<BootRecord>,
    awake_ms: u64,
    asleep_ms: u64,

    /// A watchdog on the simulation itself, not on the device.
    boot_limit_ms: u64,
}

impl World {
    pub fn new() -> Self {
        Self {
            clock: 0,
            boot_at: 0,
            soil: Script::constant(1200),
            battery: Script::constant(1400),
            button: VecDeque::new(),
            icd: IcdModel {
                lit_at: 0,
                active_until: 0,
                poll_interval_ms: tuning::LIT_POLL_PERIOD_MS,
            },
            icd_active: true,
            operating_mode_lit: true,
            icd_changed: false,
            boot_limit_ms: tuning::LIT_POLL_PERIOD_MS as u64 * 4,
            calibration: Calibration::DEFAULT,
            calibration_writes: 0,
            retained: Retained::new(
                tuning::BATTERY_HISTORY_INTERVAL_SECS,
                tuning::BATTERY_REPLACED_RISE_MV,
            ),
            leds: Vec::new(),
            soil_reports: Vec::new(),
            battery_reports: Vec::new(),
            reboot_count_writes: 0,
            active_requests: Vec::new(),
            boots: Vec::new(),
            awake_ms: 0,
            asleep_ms: 0,
        }
    }

    // --- Scenario building ---------------------------------------------------

    pub fn soil(mut self, script: Script) -> Self {
        self.soil = script;
        self
    }

    /// Change what the probe reads. Useful between boots, where a `Script` that
    /// cycles would otherwise make the next reading a guess.
    pub fn set_soil(&mut self, script: Script) {
        self.soil = script;
    }

    pub fn battery(mut self, script: Script) -> Self {
        self.battery = script;
        self
    }

    /// Operate as a short-idle device until `at_ms`, and as a long-idle one
    /// from then on - which is what a controller registering for check-ins
    /// looks like from the device's side.
    ///
    /// A boot only ends once the device is a long-idle one and idle, so a
    /// scenario that never becomes one does not finish; [`World::boot_limit`]
    /// turns that into a clear failure rather than a hang.
    pub fn lit_at(mut self, at_ms: u64) -> Self {
        self.icd.lit_at = at_ms;
        self
    }

    /// How long a single boot may last in virtual time before the simulation
    /// gives up on it. A boot that never ends means the device is staying
    /// reachable by design.
    pub fn boot_limit(mut self, ms: u64) -> Self {
        self.boot_limit_ms = ms;
        self
    }

    /// Schedule a press and release at `at_ms`.
    pub fn press(mut self, at_ms: u64, held_ms: u64) -> Self {
        self.button.push_back((at_ms, ButtonEdge::Falling));
        self.button.push_back((at_ms + held_ms, ButtonEdge::Rising));
        self
    }

    /// Pretend the device lost power: the retained store goes with it, the
    /// durable store does not.
    pub fn power_cycle(mut self) -> Self {
        self.retained = Retained::new(
            tuning::BATTERY_HISTORY_INTERVAL_SECS,
            tuning::BATTERY_REPLACED_RISE_MV,
        );
        self
    }

    // --- Running -------------------------------------------------------------

    /// Run one boot, from the current virtual time to whatever ends it.
    pub fn run_boot(&mut self, wake: WakeCause) -> Outcome {
        self.boot_at = self.clock;
        // A boot starts active, and stays active for as long as the wake phase
        // lasts: the modelled stack has not spoken yet, so there is no active
        // window to end. The window opens where the real stack starts - after
        // the wake phase, in `drive`.
        self.icd.active_until = u64::MAX;
        self.icd_active = true;
        self.icd_changed = false;
        self.operating_mode_lit = self.clock >= self.icd.lit_at;

        let started_at_ms = self.clock;
        let outcome = self.drive(wake);
        let awake_ms = self.clock - started_at_ms;

        self.awake_ms += awake_ms;
        self.boots.push(BootRecord {
            started_at_ms,
            wake,
            outcome,
            awake_ms,
        });

        if let Outcome::Sleep { for_ms } = outcome {
            self.asleep_ms += for_ms;
            self.clock += for_ms;
        }

        outcome
    }

    /// Run boots until the virtual clock passes `until_ms`.
    pub fn run_until(&mut self, until_ms: u64) {
        while self.clock < until_ms {
            // A button edge waiting at the moment of boot is what woke the
            // device; anything else is the sleep timer running out.
            let wake = match self.button.front() {
                Some((at, ButtonEdge::Falling)) if *at <= self.clock => WakeCause::Button,
                _ => WakeCause::Timer,
            };

            match self.run_boot(wake) {
                Outcome::Sleep { .. } => {}
                // A factory reset ends the simulation: the device is erased and
                // restarted, and the interesting behaviour is what led to it.
                Outcome::FactoryReset => return,
            }
        }
    }

    fn drive(&mut self, wake: WakeCause) -> Outcome {
        let cell = Box::leak(Box::new(SoilMoistureCell::new()));
        let battery_cell = Box::leak(Box::new(BatteryCell::new()));
        let soil_handler = SoilMeasurementHandler::new(1, Dataver::new(0), cell);
        let power_handler = PowerSourceHandler::new(1, Dataver::new(0), battery_cell);

        // The wake phase: the waking press and the boot's one reading, before
        // any protocol stack exists.
        let app = {
            let mut ports = SimPorts {
                world: self,
                soil_handler: &soil_handler,
                power_handler: &power_handler,
                soil_dataver: 0,
                battery_dataver: 0,
            };

            match embassy_futures::block_on(soil_sensor_core::app::run_wake(
                &mut ports,
                soil_sensor_core::app::Boot {
                    wake,
                    now: CoreInstant::ZERO,
                },
            )) {
                Ok(app) => app,
                // The only outcome a wake phase can end a boot with, and the
                // one case where the modelled stack never starts at all.
                Err(outcome) => return outcome,
            }
        };

        // The protocol stack starts here, after the wake phase - so the
        // modelled ICD's active window opens now, sized as the stack's own.
        self.icd.active_until = self.clock + tuning::ACTIVE_HOLD.as_millis();
        self.sync_icd();

        let mut ports = SimPorts {
            world: self,
            soil_handler: &soil_handler,
            power_handler: &power_handler,
            soil_dataver: 0,
            battery_dataver: 0,
        };

        embassy_futures::block_on(soil_sensor_core::app::run_boot(&mut ports, app))
    }

    // --- Observations --------------------------------------------------------

    /// Whether the device is operating as a short- or a long-idle one right
    /// now, as the world currently has it.
    pub fn icd_mode_now(&self) -> OperatingMode {
        self.operating_mode()
    }

    pub fn now_ms(&self) -> u64 {
        self.clock
    }

    pub fn boots(&self) -> &[BootRecord] {
        &self.boots
    }

    pub fn boot_count(&self) -> usize {
        self.boots.len()
    }

    pub fn awake_ms(&self) -> u64 {
        self.awake_ms
    }

    pub fn asleep_ms(&self) -> u64 {
        self.asleep_ms
    }

    pub fn leds(&self) -> &[(u64, LedCommand)] {
        &self.leds
    }

    /// The soil reports that actually reached the protocol stack, i.e. the ones
    /// the deadband let through.
    pub fn soil_reports(&self) -> &[Report] {
        &self.soil_reports
    }

    pub fn battery_reports(&self) -> &[Report] {
        &self.battery_reports
    }

    pub fn active_requests(&self) -> &[(u64, Option<Millis>)] {
        &self.active_requests
    }

    pub fn calibration(&self) -> Calibration {
        self.calibration
    }

    pub fn calibration_writes(&self) -> u32 {
        self.calibration_writes
    }

    pub fn reboot_count_writes(&self) -> u32 {
        self.reboot_count_writes
    }

    /// The retained trend, as the next boot would find it.
    pub fn retained(&self) -> &Retained {
        &self.retained
    }

    /// Absolute virtual time of the next scheduled button edge.
    fn next_button_at(&self) -> Option<u64> {
        self.button.front().map(|(at, _)| *at)
    }

    fn icd_state(&self) -> IcdState {
        let active = self.clock < self.icd.active_until;

        IcdState {
            power_mode: if active {
                PowerMode::Active
            } else {
                PowerMode::Idle
            },
            operating_mode: self.operating_mode(),
            poll_interval_ms: self.icd.poll_interval_ms,
            idle_until: (!active).then(|| {
                CoreInstant::from_millis(
                    self.icd.active_until + u64::from(tuning::LIT_POLL_PERIOD_MS) - self.boot_at,
                )
            }),
        }
    }

    /// Bring the modelled stack's state up to date with the clock, and note
    /// whether the boot has yet to hear about the change.
    fn sync_icd(&mut self) {
        let active = self.clock < self.icd.active_until;
        if active != self.icd_active {
            self.icd_active = active;
            self.icd_changed = true;
        }

        let lit = self.operating_mode() == OperatingMode::Lit;
        if lit != self.operating_mode_lit {
            self.operating_mode_lit = lit;
            self.icd_changed = true;
        }
    }

    fn operating_mode(&self) -> OperatingMode {
        if self.clock >= self.icd.lit_at {
            OperatingMode::Lit
        } else {
            OperatingMode::Sit
        }
    }

    /// The next moment the protocol stack will change its mind, if it will.
    fn next_icd_at(&self) -> Option<u64> {
        let active = (self.clock < self.icd.active_until).then_some(self.icd.active_until);
        let becomes_lit = (self.clock < self.icd.lit_at).then_some(self.icd.lit_at);

        match (active, becomes_lit) {
            (Some(a), Some(l)) => Some(a.min(l)),
            (a, l) => a.or(l),
        }
    }
}

impl Default for World {
    fn default() -> Self {
        Self::new()
    }
}

/// The ports, over a [`World`].
struct SimPorts<'a> {
    world: &'a mut World,
    soil_handler: &'a SoilMeasurementHandler<'a>,
    power_handler: &'a PowerSourceHandler<'a>,
    soil_dataver: u32,
    battery_dataver: u32,
}

impl Ports for SimPorts<'_> {
    fn now(&self) -> CoreInstant {
        CoreInstant::from_millis(self.world.clock - self.world.boot_at)
    }

    fn uptime_s(&self) -> u32 {
        // Absolute, not boot-relative: this is the clock that survives a reboot.
        (self.world.clock / 1000) as u32
    }

    async fn delay(&mut self, d: Millis) {
        // Time passes because the code asked it to, and only then.
        self.world.clock += d.as_millis();
        self.world.sync_icd();
    }

    async fn next_event(&mut self, deadline: CoreInstant) -> Event {
        // A change that happened while the boot was busy is still pending: the
        // stack's notification latches, so it is delivered on the next wait.
        self.world.sync_icd();
        if self.world.icd_changed {
            self.world.icd_changed = false;
            return Event::Icd;
        }

        let deadline_at = self.world.boot_at + deadline.as_millis();
        let button_at = self.world.next_button_at();
        let icd_at = self.world.next_icd_at();

        // Whichever comes first, and the clock moves to it.
        let next = [Some(deadline_at), button_at, icd_at]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(deadline_at);

        let before = self.world.clock;
        self.world.clock = before.max(next);

        if Some(next) == button_at {
            let (_, edge) = self.world.button.pop_front().expect("scheduled edge");
            return Event::Button(edge);
        }

        if Some(next) == icd_at {
            return Event::Icd;
        }

        // The deadline. One that has already passed means the boot loop asked to
        // wait for an instant it should have dealt with itself, and nothing would
        // ever end that wait - so it is a fault in the boot loop rather than in
        // the world, and worth saying so loudly.
        //
        // A button edge that has already happened is a different matter: a press
        // that arrived while the boot was busy is a real event, and delivering it
        // late is the point.
        assert!(
            next > before,
            "the boot loop asked to wait until {next} ms, which had already passed \
             (it was {before}) - waiting for an instant that has gone is a wait \
             nothing can end",
        );

        // A watchdog on the simulation, not on the device. A boot only ends once
        // the device is a long-idle one and idle, so a scenario that never
        // becomes one would spin here forever - and because the boot loop is
        // driven by `block_on`, it would spin at full tilt rather than waiting.
        assert!(
            self.world.clock <= self.world.boot_at + self.world.boot_limit_ms,
            "this boot has run for more than {} ms of virtual time without ending \
             - most likely the device is operating as a short-idle one, which \
             stays reachable by design and therefore never sleeps",
            self.world.boot_limit_ms,
        );

        Event::Deadline
    }

    fn try_next_button_edge(&mut self) -> Option<ButtonEdge> {
        match self.world.button.front() {
            Some((at, _)) if *at <= self.world.clock => {
                let (_, edge) = self.world.button.pop_front().expect("scheduled edge");
                Some(edge)
            }
            _ => None,
        }
    }

    fn send_led(&mut self, cmd: LedCommand) {
        self.world.leds.push((self.world.clock, cmd));
    }

    async fn read_soil_mv(&mut self) -> u32 {
        // The device layer takes real time to produce a reading, and the energy
        // budget has to see it.
        self.world.clock += tuning::SOIL_EXCITATION_SETTLE.as_millis()
            + u64::from(tuning::SOIL_SAMPLE_COUNT) * tuning::SOIL_SAMPLE_GAP.as_millis();
        self.world.sync_icd();
        self.world.soil.next()
    }

    async fn read_battery_mv(&mut self) -> u32 {
        self.world.clock +=
            u64::from(tuning::BATTERY_SAMPLE_COUNT) * tuning::BATTERY_SAMPLE_GAP.as_millis();
        self.world.battery.next()
    }

    fn icd(&self) -> IcdState {
        self.world.icd_state()
    }

    fn request_active(&mut self, hold: Option<Millis>) {
        self.world.active_requests.push((self.world.clock, hold));

        let hold = hold.unwrap_or(tuning::ACTIVE_HOLD);
        let until = self.world.clock + hold.as_millis();
        self.world.icd.active_until = self.world.icd.active_until.max(until);
        self.world.sync_icd();
    }

    fn report_soil(&mut self, percent: u8) {
        // Through the real cluster handler, so the deadband is exercised rather
        // than assumed.
        self.soil_handler.report(percent);

        let dataver = self.soil_handler.dataver();
        if dataver != self.soil_dataver {
            self.soil_dataver = dataver;
            self.world.soil_reports.push(Report {
                at_ms: self.world.clock,
                percent,
                dataver,
            });
        }
    }

    fn report_battery(&mut self, percent: u8, rest_mv: u32, time_remaining_s: Option<u32>) {
        self.power_handler
            .report(percent, rest_mv, time_remaining_s);

        let dataver = self.power_handler.dataver();
        if dataver != self.battery_dataver {
            self.battery_dataver = dataver;
            self.world.battery_reports.push(Report {
                at_ms: self.world.clock,
                percent,
                dataver,
            });
        }
    }

    async fn load_calibration(&mut self) -> Calibration {
        self.world.calibration
    }

    async fn store_calibration(&mut self, calibration: Calibration) -> bool {
        self.world.calibration = calibration;
        self.world.calibration_writes += 1;
        true
    }

    fn retained(&mut self) -> &mut Retained {
        &mut self.world.retained
    }
}

impl MatterPorts for SimPorts<'_> {
    fn persist_reboot_count(&mut self) {
        self.world.reboot_count_writes += 1;
    }
}
