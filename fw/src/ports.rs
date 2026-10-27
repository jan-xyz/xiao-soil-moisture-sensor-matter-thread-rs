//! The device layer: [`Ports`] over real peripherals.
//!
//! This is the only place where the decision layer's vocabulary meets `esp-hal`,
//! and it is deliberately dull. Every method either reads a peripheral, writes a
//! peripheral, or translates between a plain value and a protocol type. There is
//! no comparison, no threshold and no branch on anything but the shape of the
//! value - if one appears here, it belongs in `soil_sensor_core` instead.
//!
//! The one method with any judgement in it is [`Ports::next_event`], which has
//! to decide which of three things happened first. That is a property of the
//! world rather than a decision, and the simulation implements it with the same
//! meaning against a clock it controls.

use core::ops::Range;

use embassy_futures::select::{select3, Either3};
use embassy_time::{Duration, Timer};
use esp_hal::analog::adc::{Adc, AdcChannel};
use esp_hal::peripherals::ADC1;
use esp_hal::rtc_cntl::Rtc;
use esp_hal::Async;
use rs_matter_embassy::matter::dm::clusters::icd_mgmt::{Icd, IcdPowerMode, OperatingModeEnum};
use rs_matter_embassy::matter::Matter;

use soil_sensor_core::led::LedCommand;
use soil_sensor_core::ports::{
    ButtonEdge, Calibration, Event, IcdState, MatterPorts, OperatingMode, Ports, PowerMode,
    Retained,
};
use soil_sensor_core::time::{Instant as CoreInstant, Millis};
use soil_sensor_node::{PowerSourceHandler, SoilMeasurementHandler};

use crate::battery::Battery;
use crate::button::EdgeReceiver;
use crate::flash::SharedFlash;
use crate::leds::LedSender;
use crate::probe::SoilProbe;
use crate::store::{self, SharedKvStore};

pub struct EspPorts<'a, 'd, SoilPin, BatPin>
where
    SoilPin: AdcChannel,
    BatPin: AdcChannel,
{
    pub soil: SoilProbe<'a, SoilPin>,
    pub battery: Battery<'a, BatPin>,
    pub adc: Adc<'a, ADC1<'a>, Async>,
    pub leds: LedSender<'static>,
    pub edges: EdgeReceiver<'static>,
    pub rtc: &'a Rtc<'d>,
    pub icd: &'static Icd,
    pub flash: SharedFlash<'a, 'd>,
    pub calibration_range: Range<u32>,
    pub soil_handler: &'a SoilMeasurementHandler<'a>,
    pub power_handler: &'a PowerSourceHandler<'a>,
    /// The matter half of the ports: `None` for exactly as long as the boot's
    /// wake phase runs - the phase whose port set cannot reach the protocol
    /// stack - and attached between the phases, where the stack starts.
    pub(crate) matter: Option<(&'a Matter<'a>, SharedKvStore)>,
    /// When this boot began, on the executor's clock. Everything the decision
    /// layer sees is measured from here.
    pub boot_at: embassy_time::Instant,
}

impl<'a, 'd, SoilPin, BatPin> EspPorts<'a, 'd, SoilPin, BatPin>
where
    SoilPin: AdcChannel,
    BatPin: AdcChannel,
{
    /// Attach the matter half, between the boot's phases: the line where the
    /// protocol stack starts existing.
    pub fn attach(&mut self, matter: &'a Matter<'a>, kv: SharedKvStore) {
        self.matter = Some((matter, kv));
    }
}

impl<SoilPin, BatPin> Ports for EspPorts<'_, '_, SoilPin, BatPin>
where
    SoilPin: AdcChannel,
    BatPin: AdcChannel,
{
    fn now(&self) -> CoreInstant {
        CoreInstant::from_millis(
            embassy_time::Instant::now()
                .saturating_duration_since(self.boot_at)
                .as_millis(),
        )
    }

    fn uptime_s(&self) -> u32 {
        // The RTC clock, not the executor's: it keeps counting across the
        // reboots a deep sleep ends in, which is what the battery trend needs.
        self.rtc.time_since_power_up().as_secs() as u32
    }

    async fn delay(&mut self, d: Millis) {
        Timer::after(Duration::from_millis(d.as_millis())).await;
    }

    async fn next_event(&mut self, deadline: CoreInstant) -> Event {
        let remaining = deadline.saturating_since(self.now());
        let until = embassy_time::Instant::now()
            + embassy_time::Duration::from_millis(remaining.as_millis());

        // The protocol stack's power-mode notification is the application's to
        // wait on: the network driver owns `wait_net_changed`, and this is the
        // only other waiter.
        match select3(
            Timer::at(until),
            self.edges.receive(),
            self.icd.wait_power_changed(),
        )
        .await
        {
            Either3::First(()) => Event::Deadline,
            Either3::Second(edge) => Event::Button(edge),
            Either3::Third(()) => Event::Icd,
        }
    }

    fn try_next_button_edge(&mut self) -> Option<ButtonEdge> {
        self.edges.try_receive().ok()
    }

    fn send_led(&mut self, cmd: LedCommand) {
        // The LED task is a separate task precisely so that a long blink cannot
        // hold up the boot; dropping a cue is better than blocking on it.
        let _ = self.leds.try_send(cmd);
    }

    async fn read_soil_mv(&mut self) -> u32 {
        self.soil.sample_mv(&mut self.adc).await
    }

    async fn read_battery_mv(&mut self) -> u32 {
        self.battery.sample_mv(&mut self.adc).await
    }

    fn icd(&self) -> IcdState {
        let params = self.icd.net_params();

        IcdState {
            power_mode: match params.power_mode {
                IcdPowerMode::Active => PowerMode::Active,
                IcdPowerMode::Idle => PowerMode::Idle,
            },
            operating_mode: match params.operating_mode {
                OperatingModeEnum::SIT => OperatingMode::Sit,
                OperatingModeEnum::LIT => OperatingMode::Lit,
            },
            poll_interval_ms: params.poll_interval_ms,
            idle_until: self.icd.idle_until().map(|until| {
                CoreInstant::from_millis(until.saturating_duration_since(self.boot_at).as_millis())
            }),
        }
    }

    fn request_active(&mut self, hold: Option<Millis>) {
        match hold {
            Some(hold) => self
                .icd
                .request_active_for(embassy_time::Duration::from_millis(hold.as_millis())),
            None => self.icd.request_active(),
        }
    }

    fn report_soil(&mut self, percent: u8) {
        self.soil_handler.report(percent);
    }

    fn report_battery(&mut self, percent: u8, rest_mv: u32, time_remaining_s: Option<u32>) {
        self.power_handler
            .report(percent, rest_mv, time_remaining_s);
    }

    async fn load_calibration(&mut self) -> Calibration {
        store::load_calibration(self.flash, self.calibration_range.clone())
    }

    async fn store_calibration(&mut self, calibration: Calibration) -> bool {
        store::store_calibration(self.flash, self.calibration_range.clone(), calibration)
    }

    fn retained(&mut self) -> &mut Retained {
        store::trend()
    }

}

impl<SoilPin, BatPin> MatterPorts for EspPorts<'_, '_, SoilPin, BatPin>
where
    SoilPin: AdcChannel,
    BatPin: AdcChannel,
{
    fn persist_reboot_count(&mut self) {
        // The store access is built here, per call: a long-lived one would
        // borrow the stack immutably for the boot's whole length, and the
        // matter future needs to reach it too. The store itself is shared
        // behind a mutex, so a call from either side is simply serialised.
        let (matter, kv) = self
            .matter
            .as_ref()
            .expect("the matter half is attached before the operational phase");
        match matter.persist_reboot_count(matter.kv(*kv)) {
            Ok(()) => log::info!("Reboot count {} persisted", matter.reboot_count()),
            Err(e) => log::warn!("Reboot count persist failed: {e:?}"),
        }
    }
}
