//! The interactive dry/wet calibration flow.
//!
//! Red blink while the probe sits in dry air, green blink while it sits in
//! water, then two references are captured and stored. Roughly twenty seconds
//! of a person's time, which is why it owns the boot while it runs rather than
//! sharing the event loop: nothing else the device does is urgent next to a
//! user standing there holding a probe.
//!
//! The flow is a straight line over the ports, so a test can walk it with a
//! scripted probe and a clock that advances on demand, and assert both the
//! references it captured and the blinks it asked for.

use log::{info, warn};

use crate::led::{Color, LedCommand};
use crate::ports::{Calibration, Ports};
use crate::tuning;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CalibrationError {
    /// The two references were not far enough apart to be a calibration.
    SpanTooSmall,
    /// The references could not be stored; the previous ones remain in effect.
    PersistFailed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CalibrationOutcome {
    Accepted(Calibration),
    Rejected(CalibrationError),
}

/// Run the flow to completion. Returns the references that are now in effect.
///
/// A rejected calibration says so on the LEDs: the person holding the probe
/// would otherwise take the old references for the new ones.
pub async fn run<P: Ports>(ports: &mut P) -> CalibrationOutcome {
    let outcome = capture_and_store(ports).await;
    if let CalibrationOutcome::Rejected(_) = outcome {
        ports.send_led(LedCommand::calibration_rejected());
    }
    outcome
}

async fn capture_and_store<P: Ports>(ports: &mut P) -> CalibrationOutcome {
    let dry_mv = capture(ports, Color::Red).await;
    let wet_mv = capture(ports, Color::Green).await;

    if dry_mv - wet_mv < tuning::SOIL_CAL_MIN_SPAN_MV {
        warn!("Calibration rejected: dry={dry_mv} mV wet={wet_mv} mV span too small");
        return CalibrationOutcome::Rejected(CalibrationError::SpanTooSmall);
    }

    let calibration = Calibration { dry_mv, wet_mv };
    if ports.store_calibration(calibration).await {
        info!("Calibration accepted: dry={dry_mv} mV wet={wet_mv} mV");
        CalibrationOutcome::Accepted(calibration)
    } else {
        warn!("Calibration could not be stored");
        CalibrationOutcome::Rejected(CalibrationError::PersistFailed)
    }
}

/// Signal the user, let them settle the probe, then take the reference.
async fn capture<P: Ports>(ports: &mut P, color: Color) -> i32 {
    ports.send_led(LedCommand::Blink {
        color,
        count: 10,
        period: tuning::SOIL_CAL_BLINK_PERIOD,
    });
    ports.delay(tuning::SOIL_CAL_HOLD).await;
    ports.read_soil_mv().await as i32
}
