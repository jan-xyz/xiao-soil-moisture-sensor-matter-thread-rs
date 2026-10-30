//! Hand-written `ClusterHandler` for the battery-shaped `PowerSource` cluster.
//!
//! `rs_matter::dm::clusters::power_source::PowerSourceHandler` cannot be
//! reused here - its own doc comment says it implements only the minimal
//! conformant *wired* shape (`Feature::WIRED` is hardcoded, and battery
//! attributes always answer `AttributeNotFound`). This is the same handler,
//! rewritten for the `BATTERY` feature instead: same recipe (`pub use
//! decl::power_source::*` + hand-written `impl ClusterHandler`), same
//! `with!(required; ...)` idiom for the feature-conditional attributes.
//!
//! Per the CSA Power Source cluster spec, claiming `BATTERY` makes
//! `BatChargeLevel`, `BatReplacementNeeded` and `BatReplaceability`
//! mandatory (confirmed against `connectedhomeip`'s
//! `data_model/1.5/clusters/PowerSourceCluster.xml`). `BatPercentRemaining`
//! and `BatVoltage` stay optional under `BATTERY` alone, but this is the
//! whole reason to host the cluster, so they're claimed too. `BatTimeRemaining`
//! is an estimate from the voltage alone
//! ([`soil_sensor_core::battery_time_remaining_s`]); the board has no current
//! sensor.

use core::cell::RefCell;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex;
use rs_matter::dm::{ArrayAttributeRead, Cluster, Dataver, HandlerContext, ReadContext};
use rs_matter::error::Error;
use rs_matter::im::EndptId;
use rs_matter::tlv::{Nullable, TLVBuilderParent, ToTLVArrayBuilder, ToTLVBuilder, Utf8StrBuilder};
use rs_matter::utils::sync::Signal;
use rs_matter::with;

pub use rs_matter::dm::clusters::decl::power_source::*;

use soil_sensor_core::ports::BatteryReading;
use soil_sensor_core::tuning::BATTERY_REPORT_DELTA_MV;

pub const CLUSTER: Cluster<'static> = FULL_CLUSTER
    .with_features(Feature::BATTERY.bits())
    .with_attrs(with!(required;
        AttributeId::BatChargeLevel
        | AttributeId::BatReplacementNeeded
        | AttributeId::BatReplaceability
        | AttributeId::BatPercentRemaining
        | AttributeId::BatVoltage
        | AttributeId::BatTimeRemaining
    ))
    .with_cmds(with!());

/// Charge-level thresholds. Matches the C++ original's battery-percent
/// mapping closely enough to be a sane default; not spec-mandated.
const CHARGE_LEVEL_WARNING_BELOW_PERCENT: u8 = 20;
const CHARGE_LEVEL_CRITICAL_BELOW_PERCENT: u8 = 5;

struct Reading {
    percent: Option<u8>,
    rest_mv: Option<u32>,
    time_remaining_s: Option<u32>,
}

/// The last-reported battery reading, or `None`s if never reported. Shared
/// between the sampling loop (writer) and the cluster handler (reader).
pub struct BatteryCell(Mutex<CriticalSectionRawMutex, RefCell<Reading>>);

impl BatteryCell {
    pub const fn new() -> Self {
        Self(Mutex::new(RefCell::new(Reading {
            percent: None,
            rest_mv: None,
            time_remaining_s: None,
        })))
    }

    fn get(&self) -> Reading {
        self.0.lock(|cell| {
            let reading = cell.borrow();
            Reading {
                percent: reading.percent,
                rest_mv: reading.rest_mv,
                time_remaining_s: reading.time_remaining_s,
            }
        })
    }

    fn set(&self, reading: Option<BatteryReading>) {
        self.0.lock(|cell| {
            *cell.borrow_mut() = Reading {
                percent: reading.map(|reading| reading.percent),
                rest_mv: reading.map(|reading| reading.rest_mv),
                time_remaining_s: reading.and_then(|reading| reading.time_remaining_s),
            }
        });
    }
}

impl Default for BatteryCell {
    fn default() -> Self {
        Self::new()
    }
}

pub struct PowerSourceHandler<'a> {
    endpoint_id: EndptId,
    dataver: Dataver,
    reading: &'a BatteryCell,
    // See `soil_measurement::SoilMeasurementHandler`'s field of the same
    // name: `Dataver::changed()` alone does not push a subscription report,
    // only `HandlerContext::notify_attr_changed` from inside `run()` does -
    // this wakes `run()` to call it.
    changed: Signal<Option<()>>,
}

impl<'a> PowerSourceHandler<'a> {
    pub const fn new(endpoint_id: EndptId, dataver: Dataver, reading: &'a BatteryCell) -> Self {
        Self {
            endpoint_id,
            dataver,
            reading,
            changed: Signal::new(None),
        }
    }

    /// Called by the sampling loop with a freshly measured resting voltage,
    /// the derived percent and the estimated time remaining, or `None` when
    /// there is no cell. Only reports (stores the reading, bumps the dataver
    /// and wakes `run()` to push a subscription update) when the voltage moved
    /// by at least `BATTERY_REPORT_DELTA_MV` since the last report, or when an
    /// estimate or the cell becomes available or unavailable, so ADC jitter
    /// does not wake the Thread radio but a slow discharge still reaches the
    /// controller.
    pub fn report(&self, reading: Option<BatteryReading>) {
        let reported = self.reading.get();
        let should_report = match reading {
            Some(reading) => {
                soil_sensor_core::should_report(
                    reported.rest_mv,
                    reading.rest_mv,
                    BATTERY_REPORT_DELTA_MV as u32,
                ) || reported.time_remaining_s.is_some() != reading.time_remaining_s.is_some()
            }
            None => reported.rest_mv.is_some(),
        };

        if should_report {
            self.reading.set(reading);
        }

        match reading {
            Some(BatteryReading {
                percent,
                rest_mv,
                time_remaining_s,
            }) => log::info!(
                "Battery: {percent}% ({rest_mv} mV, time remaining: {time_remaining_s:?} s, reported change: {should_report})"
            ),
            None => log::info!("Battery: unknown (reported change: {should_report})"),
        }

        if should_report {
            self.dataver.changed();
            self.changed.signal(());
        }
    }
}

impl ClusterHandler for PowerSourceHandler<'_> {
    const CLUSTER: Cluster<'static> = CLUSTER;

    fn dataver(&self) -> u32 {
        self.dataver.get()
    }

    fn dataver_changed(&self) {
        self.dataver.changed();
    }

    async fn run(&self, ctx: impl HandlerContext) -> Result<(), Error> {
        loop {
            self.changed.wait_signalled().await;
            ctx.notify_attr_changed(
                self.endpoint_id,
                Self::CLUSTER.id,
                AttributeId::BatPercentRemaining as _,
            );
            ctx.notify_attr_changed(
                self.endpoint_id,
                Self::CLUSTER.id,
                AttributeId::BatVoltage as _,
            );
            ctx.notify_attr_changed(
                self.endpoint_id,
                Self::CLUSTER.id,
                AttributeId::BatTimeRemaining as _,
            );
        }
    }

    fn status(&self, _ctx: impl ReadContext) -> Result<PowerSourceStatusEnum, Error> {
        Ok(PowerSourceStatusEnum::Active)
    }

    fn order(&self, _ctx: impl ReadContext) -> Result<u8, Error> {
        Ok(0)
    }

    fn description<P: TLVBuilderParent>(
        &self,
        _ctx: impl ReadContext,
        out: Utf8StrBuilder<P>,
    ) -> Result<P, Error> {
        out.set("AA Battery")
    }

    fn endpoint_list<P: TLVBuilderParent>(
        &self,
        _ctx: impl ReadContext,
        builder: ArrayAttributeRead<ToTLVArrayBuilder<P, EndptId>, ToTLVBuilder<P, EndptId>>,
    ) -> Result<P, Error> {
        // Empty per spec means "powers the node as a whole", which is our
        // case - there is only one endpoint.
        match builder {
            ArrayAttributeRead::ReadAll(builder) => builder.end(),
            ArrayAttributeRead::ReadOne(_, _) => {
                Err(rs_matter::error::ErrorCode::ConstraintError.into())
            }
            ArrayAttributeRead::ReadNone(builder) => builder.end(),
        }
    }

    fn bat_percent_remaining(&self, _ctx: impl ReadContext) -> Result<Nullable<u8>, Error> {
        // Half-percent units per spec (0-200 represents 0-100%).
        Ok(match self.reading.get().percent {
            Some(percent) => Nullable::some(percent.saturating_mul(2)),
            None => Nullable::none(),
        })
    }

    fn bat_voltage(&self, _ctx: impl ReadContext) -> Result<Nullable<u32>, Error> {
        Ok(match self.reading.get().rest_mv {
            Some(mv) => Nullable::some(mv),
            None => Nullable::none(),
        })
    }

    fn bat_time_remaining(&self, _ctx: impl ReadContext) -> Result<Nullable<u32>, Error> {
        Ok(match self.reading.get().time_remaining_s {
            Some(time_remaining_s) => Nullable::some(time_remaining_s),
            None => Nullable::none(),
        })
    }

    fn bat_charge_level(&self, _ctx: impl ReadContext) -> Result<BatChargeLevelEnum, Error> {
        Ok(match self.reading.get().percent {
            Some(percent) if percent < CHARGE_LEVEL_CRITICAL_BELOW_PERCENT => {
                BatChargeLevelEnum::Critical
            }
            Some(percent) if percent < CHARGE_LEVEL_WARNING_BELOW_PERCENT => {
                BatChargeLevelEnum::Warning
            }
            _ => BatChargeLevelEnum::OK,
        })
    }

    fn bat_replacement_needed(&self, _ctx: impl ReadContext) -> Result<bool, Error> {
        Ok(
            matches!(self.reading.get().percent, Some(percent) if percent < CHARGE_LEVEL_CRITICAL_BELOW_PERCENT),
        )
    }

    fn bat_replaceability(&self, _ctx: impl ReadContext) -> Result<BatReplaceabilityEnum, Error> {
        // A user swaps the AA cell directly - no tools, no factory visit.
        Ok(BatReplaceabilityEnum::UserReplaceable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A handler over a cell of its own.
    ///
    /// The cell has to be per-handler: the last reported value lives in it, and
    /// a shared one would let one test's reports suppress another's.
    fn handler() -> PowerSourceHandler<'static> {
        let cell: &'static BatteryCell = Box::leak(Box::new(BatteryCell::new()));
        PowerSourceHandler::new(1, Dataver::new(0), cell)
    }

    fn reading(percent: u8, rest_mv: u32, time_remaining_s: Option<u32>) -> Option<BatteryReading> {
        Some(BatteryReading {
            percent,
            rest_mv,
            time_remaining_s,
        })
    }

    #[test]
    fn the_first_reading_always_reports() {
        let handler = handler();
        let before = handler.dataver();

        handler.report(reading(80, 1400, None));

        assert_ne!(handler.dataver(), before);
    }

    #[test]
    fn voltage_jitter_below_the_deadband_is_not_reported() {
        let handler = handler();
        handler.report(reading(80, 1400, None));
        let reported = handler.dataver();

        handler.report(reading(80, 1410, None));
        assert_eq!(
            handler.dataver(),
            reported,
            "ten millivolts is converter noise at this scale"
        );
    }

    #[test]
    fn a_reading_that_moved_enough_is_reported() {
        let handler = handler();
        handler.report(reading(80, 1400, None));
        let reported = handler.dataver();

        handler.report(reading(79, 1380, None));
        assert_ne!(handler.dataver(), reported);
    }

    #[test]
    fn an_estimate_appearing_on_its_own_is_reported() {
        let handler = handler();
        handler.report(reading(80, 1400, None));
        let reported = handler.dataver();

        handler.report(reading(80, 1400, Some(3_600)));
        assert_ne!(
            handler.dataver(),
            reported,
            "a new time-remaining estimate is news even when the voltage has not moved"
        );
    }

    #[test]
    fn a_cell_taken_out_is_reported_as_unknown() {
        let handler = handler();
        handler.report(reading(80, 1400, Some(3_600)));
        let reported = handler.dataver();

        handler.report(None);
        assert_ne!(handler.dataver(), reported);
        assert_eq!(
            (
                handler.reading.get().percent,
                handler.reading.get().rest_mv,
                handler.reading.get().time_remaining_s
            ),
            (None, None, None)
        );
    }

    #[test]
    fn no_cell_from_the_start_is_not_news() {
        let handler = handler();
        let before = handler.dataver();

        handler.report(None);
        assert_eq!(handler.dataver(), before, "the attributes are already null");
    }
}
