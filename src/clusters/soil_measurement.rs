//! Hand-written `ClusterHandler` for the `SoilMeasurement` cluster.
//!
//! `rs-matter` code-generates the raw attribute plumbing for this cluster
//! (it is in the bundled Matter IDL, just marked `provisional`) but ships no
//! ergonomic handler for it the way it does for `Descriptor` or `OnOff`. This
//! follows the same recipe `rs_matter::dm::clusters::desc::DescHandler` uses:
//! `pub use decl::soil_measurement::*` for the generated types, then a
//! hand-written `impl ClusterHandler`.

use core::cell::RefCell;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex;
use rs_matter_embassy::matter::dm::clusters::decl::globals::{
    MeasurementAccuracyStructBuilder, MeasurementTypeEnum,
};
use rs_matter_embassy::matter::dm::{Dataver, HandlerContext, ReadContext};
use rs_matter_embassy::matter::error::Error;
use rs_matter_embassy::matter::im::{EndptId, Percent};
use rs_matter_embassy::matter::tlv::{Nullable, TLVBuilderParent};
use rs_matter_embassy::matter::utils::sync::Signal;

pub use rs_matter_embassy::matter::dm::clusters::decl::soil_measurement::*;

use crate::pins::SOIL_REPORT_DELTA_PERCENT;

/// The last-reported moisture percent, or `None` if the probe has not
/// sampled yet. Shared between the sampling loop (writer) and the cluster
/// handler (reader) - both run synchronously with respect to each other
/// (no `.await` needed to touch it), so a blocking mutex is enough.
pub struct SoilMoistureCell(Mutex<CriticalSectionRawMutex, RefCell<Option<u8>>>);

impl SoilMoistureCell {
    pub const fn new() -> Self {
        Self(Mutex::new(RefCell::new(None)))
    }

    fn get(&self) -> Option<u8> {
        self.0.lock(|cell| *cell.borrow())
    }

    fn set(&self, value: u8) {
        self.0.lock(|cell| *cell.borrow_mut() = Some(value));
    }
}

impl Default for SoilMoistureCell {
    fn default() -> Self {
        Self::new()
    }
}

pub struct SoilMeasurementHandler<'a> {
    endpoint_id: EndptId,
    dataver: Dataver,
    value: &'a SoilMoistureCell,
    // `Dataver::changed()` alone does not push a subscription report - it
    // only affects what a *fresh read* sees. Live updates require calling
    // `HandlerContext::notify_attr_changed` from inside `run()`, which only
    // has a `ctx` to call it with while it's executing - so `report()` (sync,
    // called from the sampling loop, no `ctx` in scope) just wakes `run()`
    // via this signal, mirroring `rs_matter::dm::clusters::app::on_off`'s
    // `state_change_signal` pattern.
    changed: Signal<Option<()>>,
}

impl<'a> SoilMeasurementHandler<'a> {
    pub const fn new(endpoint_id: EndptId, dataver: Dataver, value: &'a SoilMoistureCell) -> Self {
        Self {
            endpoint_id,
            dataver,
            value,
            changed: Signal::new(None),
        }
    }

    /// Called by the sampling loop with a freshly measured percent. Only
    /// reports (bumps the dataver and wakes `run()` to push a subscription
    /// update) when the reading moved by at least `SOIL_REPORT_DELTA_PERCENT`,
    /// so ADC jitter does not wake the Thread radio.
    pub fn report(&self, percent: u8) {
        let should_report = match self.value.get() {
            Some(previous) => previous.abs_diff(percent) >= SOIL_REPORT_DELTA_PERCENT,
            None => true,
        };

        self.value.set(percent);

        if should_report {
            self.dataver.changed();
            self.changed.signal(());
        }

        log::info!("Soil moisture: {percent}% (reported change: {should_report})");
    }
}

impl ClusterHandler for SoilMeasurementHandler<'_> {
    const CLUSTER: rs_matter_embassy::matter::dm::Cluster<'static> = FULL_CLUSTER;

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
                AttributeId::SoilMoistureMeasuredValue as _,
            );
        }
    }

    fn soil_moisture_measurement_limits<P: TLVBuilderParent>(
        &self,
        _ctx: impl ReadContext,
        builder: MeasurementAccuracyStructBuilder<P>,
    ) -> Result<P, Error> {
        // +-5% accuracy (Percent100ths units) over the full 0-100% range -
        // new to this port (the C++ original predates this cluster's
        // exposure in rs-matter and does not report this attribute).
        builder
            .measurement_type(MeasurementTypeEnum::SoilMoisture)?
            .measured(true)?
            .min_measured_value(0)?
            .max_measured_value(100)?
            .accuracy_ranges()?
            .push()?
            .range_min(0)?
            .range_max(100)?
            .percent_max(Some(500))?
            .percent_min(None)?
            .percent_typical(None)?
            .fixed_max(None)?
            .fixed_min(None)?
            .fixed_typical(None)?
            .end()? // closes the range struct, back to the array builder
            .end()? // closes the array, back to the outer struct builder
            .end() // closes the outer struct, back to P
    }

    fn soil_moisture_measured_value(
        &self,
        _ctx: impl ReadContext,
    ) -> Result<Nullable<Percent>, Error> {
        Ok(match self.value.get() {
            Some(percent) => Nullable::some(percent),
            None => Nullable::none(),
        })
    }
}
