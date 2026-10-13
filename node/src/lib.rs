//! The integration layer: the Matter node.
//!
//! Endpoints, cluster handlers and the small amount of protocol glue that turns
//! the decision layer's intent ("report this moisture reading") into Matter
//! vocabulary (a cluster attribute, a dataver bump, a subscription report).
//!
//! Nothing here touches hardware, so all of it builds - and is tested - on the
//! host. The one protocol-specific thing it needs, `std` and the OS networking
//! that the stack's `os` feature pulls in, is deliberately left off: only the
//! simulation crate may turn that on, and only for the host. See
//! `ARCHITECTURE.md`.

#![cfg_attr(not(test), no_std)]

pub mod devices;
pub mod power_source;
pub mod soil_measurement;

pub use devices::DEV_TYPE_SOIL_SENSOR;
pub use power_source::{BatteryCell, PowerSourceHandler};
pub use soil_measurement::{SoilMeasurementHandler, SoilMoistureCell};
