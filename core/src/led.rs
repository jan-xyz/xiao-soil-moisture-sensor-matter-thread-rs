//! The status LEDs, as the decision layer sees them: a command, not a pin.

use crate::{time::Millis, tuning};

/// One of the three status LEDs, named after its colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Color {
    /// Dry soil, and the dry step of the calibration.
    Red,
    /// Moist soil, and a rejected calibration.
    Yellow,
    /// Wet soil, and the wet step of the calibration.
    Green,
}

/// What to show. The device layer turns this into pin writes; the decision
/// layer only ever says *what*, never *how long the pin is high*.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LedCommand {
    /// `count` on/off cycles of `color`, each phase `period` long.
    Blink {
        /// The LED to blink.
        color: Color,
        /// How many on/off cycles.
        count: u32,
        /// How long each on and each off phase lasts.
        period: Millis,
    },
    /// One long blink of the colour classifying `moisture_percent`.
    ClassifyMoisture {
        /// The reading to classify, in percent.
        moisture_percent: u8,
    },
}

impl LedCommand {
    /// The one long blink that says how wet the soil is.
    #[must_use]
    pub fn classify(moisture_percent: u8) -> Self {
        Self::ClassifyMoisture { moisture_percent }
    }

    /// The boot before this one crashed.
    #[must_use]
    pub fn crashed() -> Self {
        Self::error(Color::Red)
    }

    /// The calibration flow ended without new references, so the old ones
    /// are still in effect.
    #[must_use]
    pub fn calibration_rejected() -> Self {
        Self::error(Color::Yellow)
    }

    fn error(color: Color) -> Self {
        Self::Blink {
            color,
            count: tuning::ERROR_BLINK_COUNT,
            period: tuning::ERROR_BLINK_PERIOD,
        }
    }

    /// Red = dry, yellow = almost dry, green = normal.
    #[must_use]
    pub fn color_for(moisture_percent: u8) -> Color {
        if moisture_percent < tuning::MOISTURE_DRY_BELOW_PERCENT {
            Color::Red
        } else if moisture_percent < tuning::MOISTURE_NORMAL_ABOVE_PERCENT {
            Color::Yellow
        } else {
            Color::Green
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_follows_the_thresholds() {
        assert_eq!(LedCommand::color_for(0), Color::Red);
        assert_eq!(LedCommand::color_for(22), Color::Red);
        assert_eq!(LedCommand::color_for(23), Color::Yellow);
        assert_eq!(LedCommand::color_for(57), Color::Yellow);
        assert_eq!(LedCommand::color_for(58), Color::Green);
        assert_eq!(LedCommand::color_for(100), Color::Green);
    }
}
