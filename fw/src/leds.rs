//! The status LEDs.
//!
//! A task of its own, because a blink outlives the moment that asked for it: a
//! twenty-blink calibration cue takes ten seconds, and the boot must carry on
//! meanwhile. The decision layer says *what* to show; this says how long a pin
//! is high.

use embassy_executor::task;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Channel, Receiver, Sender};
use embassy_time::{Duration, Timer};
use esp_hal::gpio::Output;

use soil_sensor_core::led::{Color, LedCommand};
use soil_sensor_core::time::Millis;

const LED_QUEUE_DEPTH: usize = 4;
pub type LedChannel = Channel<CriticalSectionRawMutex, LedCommand, LED_QUEUE_DEPTH>;
pub type LedSender<'a> = Sender<'a, CriticalSectionRawMutex, LedCommand, LED_QUEUE_DEPTH>;
pub type LedReceiver<'a> = Receiver<'a, CriticalSectionRawMutex, LedCommand, LED_QUEUE_DEPTH>;

/// One long blink of the colour that classifies a moisture reading.
const CLASSIFY_BLINK: Millis = Millis::from_millis(1_000);

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

    async fn blink(&mut self, color: Color, count: u32, period: Millis) {
        self.all_off();
        for _ in 0..count {
            self.pin(color).set_high();
            Timer::after(Duration::from_millis(period.as_millis())).await;
            self.pin(color).set_low();
            Timer::after(Duration::from_millis(period.as_millis())).await;
        }
    }

    /// Runs forever, serializing whatever command arrives next, so two producers
    /// can never fight over the pins.
    pub async fn run(mut self, commands: LedReceiver<'static>) -> ! {
        loop {
            match commands.receive().await {
                LedCommand::Blink {
                    color,
                    count,
                    period,
                } => self.blink(color, count, period).await,
                LedCommand::ClassifyMoisture { moisture_percent } => {
                    self.blink(LedCommand::color_for(moisture_percent), 1, CLASSIFY_BLINK)
                        .await;
                }
            }
        }
    }
}

/// The LEDs task, spawned at the very start of the boot rather than run under
/// the boot loop's select: a blink outlives the moment that asked for it, and
/// the wake phase's classification has to render before the protocol stack
/// even exists.
#[task]
pub async fn run(leds: StatusLeds<'static>, commands: LedReceiver<'static>) {
    leds.run(commands).await
}
