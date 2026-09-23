//! Three status LEDs (red/yellow/green), driven by a single task.
//!
//! Replaces `status_led.cpp`. The C++ original serializes blink patterns
//! through one esp_timer callback because only one pattern should be
//! visually active at a time; the embassy-native equivalent is a single task
//! that owns all three `Output` pins and a channel of commands, so producers
//! (the button task, the sampling loop) never fight over the GPIOs directly.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Channel, Receiver, Sender};
use embassy_time::{Duration, Timer};
use esp_hal::gpio::Output;

use crate::pins::{MOISTURE_DRY_BELOW_PERCENT, MOISTURE_NORMAL_ABOVE_PERCENT};

#[derive(Clone, Copy, Debug)]
pub enum Color {
    Red,
    Yellow,
    Green,
}

#[derive(Clone, Copy, Debug)]
pub enum LedCommand {
    /// Blink `count` on/off cycles of `color`, each phase `period` long.
    Blink { color: Color, count: u32, period: Duration },
    /// One long blink of the color classifying `moisture_percent`: red =
    /// dry, yellow = almost dry, green = normal. Same thresholds as the
    /// original ESPHome firmware.
    ClassifyMoisture { moisture_percent: u8 },
}

const LED_QUEUE_DEPTH: usize = 4;
pub type LedChannel = Channel<CriticalSectionRawMutex, LedCommand, LED_QUEUE_DEPTH>;
pub type LedSender<'a> = Sender<'a, CriticalSectionRawMutex, LedCommand, LED_QUEUE_DEPTH>;
type LedReceiver<'a> = Receiver<'a, CriticalSectionRawMutex, LedCommand, LED_QUEUE_DEPTH>;

pub struct StatusLeds<'d> {
    red: Output<'d>,
    yellow: Output<'d>,
    green: Output<'d>,
}

impl<'d> StatusLeds<'d> {
    pub fn new(red: Output<'d>, yellow: Output<'d>, green: Output<'d>) -> Self {
        Self { red, yellow, green }
    }

    fn pin(&mut self, color: Color) -> &mut Output<'d> {
        match color {
            Color::Red => &mut self.red,
            Color::Yellow => &mut self.yellow,
            Color::Green => &mut self.green,
        }
    }

    fn all_off(&mut self) {
        self.red.set_low();
        self.yellow.set_low();
        self.green.set_low();
    }

    async fn blink(&mut self, color: Color, count: u32, period: Duration) {
        self.all_off();
        for _ in 0..count {
            self.pin(color).set_high();
            Timer::after(period).await;
            self.pin(color).set_low();
            Timer::after(period).await;
        }
    }

    /// Runs forever, serializing whatever [`LedCommand`] arrives next.
    pub async fn run(mut self, commands: LedReceiver<'_>) -> ! {
        loop {
            match commands.receive().await {
                LedCommand::Blink { color, count, period } => {
                    self.blink(color, count, period).await;
                }
                LedCommand::ClassifyMoisture { moisture_percent } => {
                    let color = if moisture_percent < MOISTURE_DRY_BELOW_PERCENT {
                        Color::Red
                    } else if moisture_percent < MOISTURE_NORMAL_ABOVE_PERCENT {
                        Color::Yellow
                    } else {
                        Color::Green
                    };
                    self.blink(color, 1, Duration::from_secs(1)).await;
                }
            }
        }
    }
}
