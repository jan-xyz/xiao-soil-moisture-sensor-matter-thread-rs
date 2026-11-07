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
//! and `BatVoltage` stay optional under `BATTERY` alone, but they are the
//! whole reason to host the cluster, so they're claimed too. `BatTimeRemaining`
//! is *not* claimed: an estimate from the voltage alone has no current draw to
//! divide by, and the attribute would only add report weight (and a fiction)
//! for a controller that cannot act on it anyway.

use core::cell::Cell;

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
use soil_sensor_core::tuning::{BATTERY_REPORT_DELTA_HALF_PERCENT, BATTERY_REPORT_DELTA_MV};

pub const CLUSTER: Cluster<'static> = FULL_CLUSTER
    .with_features(Feature::BATTERY.bits())
    .with_attrs(with!(required;
        AttributeId::BatChargeLevel
        | AttributeId::BatReplacementNeeded
        | AttributeId::BatReplaceability
        | AttributeId::BatPercentRemaining
        | AttributeId::BatVoltage
    ))
    .with_cmds(with!());

/// Charge-level thresholds. Matches the C++ original's battery-percent
/// mapping closely enough to be a sane default; not spec-mandated.
const CHARGE_LEVEL_WARNING_BELOW_PERCENT: u8 = 20;
const CHARGE_LEVEL_CRITICAL_BELOW_PERCENT: u8 = 5;

/// The charge level of a reading, or `OK` for no cell: an empty holder on USB
/// power is not a battery to warn about.
fn charge_level(reading: Option<BatteryReading>) -> BatChargeLevelEnum {
    match reading.map(|reading| reading.half_percent) {
        Some(half_percent) if half_percent < 2 * CHARGE_LEVEL_CRITICAL_BELOW_PERCENT => {
            BatChargeLevelEnum::Critical
        }
        Some(half_percent) if half_percent < 2 * CHARGE_LEVEL_WARNING_BELOW_PERCENT => {
            BatChargeLevelEnum::Warning
        }
        _ => BatChargeLevelEnum::OK,
    }
}

/// The last-reported battery reading, or `None` if never reported or there is
/// no cell. Shared between the sampling loop (writer) and the cluster handler
/// (reader).
pub struct BatteryCell(Mutex<CriticalSectionRawMutex, Cell<Option<BatteryReading>>>);

impl BatteryCell {
    pub const fn new() -> Self {
        Self(Mutex::new(Cell::new(None)))
    }

    fn get(&self) -> Option<BatteryReading> {
        self.0.lock(Cell::get)
    }

    fn set(&self, reading: Option<BatteryReading>) {
        self.0.lock(|cell| cell.set(reading));
    }
}

impl Default for BatteryCell {
    fn default() -> Self {
        Self::new()
    }
}

/// The attributes one reading sets, and so the ones a report has to name.
const READING_ATTRIBUTES: [AttributeId; 4] = [
    AttributeId::BatPercentRemaining,
    AttributeId::BatVoltage,
    AttributeId::BatChargeLevel,
    AttributeId::BatReplacementNeeded,
];

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

    /// Called by the sampling loop with a fresh estimate, or `None` when there
    /// is no cell. Only reports (stores the reading, bumps the dataver and
    /// wakes `run()` to push a subscription update) when the voltage moved by
    /// at least `BATTERY_REPORT_DELTA_MV` since the last report, the charge by
    /// at least `BATTERY_REPORT_DELTA_HALF_PERCENT`, the charge level changed,
    /// or the cell became available or unavailable. ADC jitter does not wake
    /// the Thread radio, but a slow discharge still reaches the controller.
    ///
    /// The voltage alone is not enough: the battery curve is steep enough that
    /// a change inside the voltage deadband can cross a charge-level threshold.
    pub fn report(&self, reading: Option<BatteryReading>) {
        let reported = self.reading.get();
        let should_report = match (reported, reading) {
            (Some(reported), Some(reading)) => {
                soil_sensor_core::should_report(
                    Some(reported.rest_mv),
                    reading.rest_mv,
                    BATTERY_REPORT_DELTA_MV as u32,
                ) || soil_sensor_core::should_report(
                    Some(u32::from(reported.half_percent)),
                    u32::from(reading.half_percent),
                    u32::from(BATTERY_REPORT_DELTA_HALF_PERCENT),
                ) || charge_level(Some(reported)) != charge_level(Some(reading))
            }
            (None, None) => false,
            _ => true,
        };

        if should_report {
            self.reading.set(reading);
        }

        match reading {
            Some(BatteryReading {
                half_percent,
                rest_mv,
            }) => {
                let percent = half_percent / 2;
                let half = if half_percent % 2 == 0 { "" } else { ".5" };
                log::info!(
                    "Battery: {percent}{half}% ({rest_mv} mV, reported change: {should_report})"
                )
            }
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
            for attribute in READING_ATTRIBUTES {
                ctx.notify_attr_changed(self.endpoint_id, Self::CLUSTER.id, attribute as _);
            }
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
        Ok(match self.reading.get() {
            Some(reading) => Nullable::some(reading.half_percent),
            None => Nullable::none(),
        })
    }

    fn bat_voltage(&self, _ctx: impl ReadContext) -> Result<Nullable<u32>, Error> {
        Ok(match self.reading.get() {
            Some(reading) => Nullable::some(reading.rest_mv),
            None => Nullable::none(),
        })
    }

    fn bat_charge_level(&self, _ctx: impl ReadContext) -> Result<BatChargeLevelEnum, Error> {
        Ok(charge_level(self.reading.get()))
    }

    fn bat_replacement_needed(&self, _ctx: impl ReadContext) -> Result<bool, Error> {
        Ok(charge_level(self.reading.get()) == BatChargeLevelEnum::Critical)
    }

    fn bat_replaceability(&self, _ctx: impl ReadContext) -> Result<BatReplaceabilityEnum, Error> {
        // A user swaps the AA cell directly - no tools, no factory visit.
        Ok(BatReplaceabilityEnum::UserReplaceable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(half_percent: u8, rest_mv: u32) -> Option<BatteryReading> {
        Some(BatteryReading {
            half_percent,
            rest_mv,
        })
    }

    struct ReportTestCase {
        name: &'static str,
        earlier: Option<Option<BatteryReading>>,
        input: Option<BatteryReading>,
        expected_reported: bool,
        expected_stored: Option<BatteryReading>,
    }

    #[test]
    fn test_report() {
        let test_cases = vec![
            ReportTestCase {
                name: "the first reading always reports",
                earlier: None,
                input: reading(160, 1400),
                expected_reported: true,
                expected_stored: reading(160, 1400),
            },
            ReportTestCase {
                name: "ten millivolts is converter noise at this scale",
                earlier: Some(reading(160, 1400)),
                input: reading(160, 1410),
                expected_reported: false,
                expected_stored: reading(160, 1400),
            },
            ReportTestCase {
                name: "a voltage that moved enough is reported",
                earlier: Some(reading(160, 1400)),
                input: reading(158, 1380),
                expected_reported: true,
                expected_stored: reading(158, 1380),
            },
            ReportTestCase {
                name: "a charge that moved enough is reported inside the voltage deadband",
                earlier: Some(reading(80, 1260)),
                input: reading(70, 1250),
                expected_reported: true,
                expected_stored: reading(70, 1250),
            },
            ReportTestCase {
                name: "a crossing into the warning level is reported",
                earlier: Some(reading(41, 1219)),
                input: reading(39, 1218),
                expected_reported: true,
                expected_stored: reading(39, 1218),
            },
            ReportTestCase {
                name: "a small change inside one charge level is not reported",
                earlier: Some(reading(39, 1218)),
                input: reading(37, 1215),
                expected_reported: false,
                expected_stored: reading(39, 1218),
            },
            ReportTestCase {
                name: "a cell taken out is reported as unknown",
                earlier: Some(reading(160, 1400)),
                input: None,
                expected_reported: true,
                expected_stored: None,
            },
            ReportTestCase {
                name: "no cell from the start is not news, the attributes are already null",
                earlier: None,
                input: None,
                expected_reported: false,
                expected_stored: None,
            },
        ];

        for ReportTestCase {
            name,
            earlier,
            input,
            expected_reported,
            expected_stored,
        } in test_cases
        {
            // The cell is per case: the last reported value lives in it, and a
            // shared one would let one case's reports suppress another's.
            let cell: &'static BatteryCell = Box::leak(Box::new(BatteryCell::new()));
            let handler = PowerSourceHandler::new(1, Dataver::new(0), cell);
            if let Some(earlier) = earlier {
                handler.report(earlier);
            }
            let before = handler.dataver();

            handler.report(input);

            let result = (handler.dataver() != before, cell.get());
            assert_eq!(
                result,
                (expected_reported, expected_stored),
                "Failed case: '{name}'"
            );
        }
    }

    struct ChargeLevelTestCase {
        name: &'static str,
        input: Option<BatteryReading>,
        expected: BatChargeLevelEnum,
    }

    #[test]
    fn test_charge_level() {
        let test_cases = vec![
            ChargeLevelTestCase {
                name: "no cell is not a battery to warn about",
                input: None,
                expected: BatChargeLevelEnum::OK,
            },
            ChargeLevelTestCase {
                name: "20% is still OK",
                input: reading(40, 1220),
                expected: BatChargeLevelEnum::OK,
            },
            ChargeLevelTestCase {
                name: "19.5% is a warning",
                input: reading(39, 1219),
                expected: BatChargeLevelEnum::Warning,
            },
            ChargeLevelTestCase {
                name: "5% is still a warning",
                input: reading(10, 1170),
                expected: BatChargeLevelEnum::Warning,
            },
            ChargeLevelTestCase {
                name: "4.5% is critical",
                input: reading(9, 1165),
                expected: BatChargeLevelEnum::Critical,
            },
        ];

        for ChargeLevelTestCase {
            name,
            input,
            expected,
        } in test_cases
        {
            let result = charge_level(input);
            assert_eq!(result, expected, "Failed case: '{name}'");
        }
    }
}
