//! The status LEDs, as the decision layer sees them: a command, not a pin.

use crate::time::Millis;
use crate::tuning;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Color {
    Red,
    Yellow,
    Green,
}

/// What to show. The device layer turns this into pin writes; the decision
/// layer only ever says *what*, never *how long the pin is high*.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LedCommand {
    /// `count` on/off cycles of `color`, each phase `period` long.
    Blink {
        color: Color,
        count: u32,
        period: Millis,
    },
    /// One long blink of the colour classifying `moisture_percent`.
    ClassifyMoisture { moisture_percent: u8 },
}

impl LedCommand {
    /// The one long blink that says how wet the soil is.
    pub fn classify(moisture_percent: u8) -> Self {
        Self::ClassifyMoisture { moisture_percent }
    }

    /// Something went wrong that a person should know about.
    pub fn error() -> Self {
        Self::Blink {
            color: Color::Red,
            count: tuning::ERROR_BLINK_COUNT,
            period: tuning::ERROR_BLINK_PERIOD,
        }
    }

    /// Red = dry, yellow = almost dry, green = normal.
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
