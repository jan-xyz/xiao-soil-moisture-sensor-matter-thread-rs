//! The user button, as a source of edges.
//!
//! Deliberately the thinnest possible task: it owns the pin, keeps it listening,
//! and turns edges into messages. Everything that decides what a gesture *means*
//! is in the decision layer, where it can be tested.
//!
//! Two details are load-bearing:
//!
//! - the pin is configured for the chip's low-power pad path, because deep sleep
//!   powers down the GPIO peripheral and a wake source has to survive that. Only
//!   GPIO0..7 have such a path, which is why the button is not on the module's
//!   BOOT pin.
//! - a deep-sleep wake is a reboot that the button caused, so the chip comes
//!   back up with the button already down and no falling edge pending. The first
//!   thing the loop does is emit an edge for the press that is already happening.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Channel, Receiver, Sender};
use esp_hal::gpio::{Flex, InputConfig, Pull, WakeupConfig};

use soil_sensor_core::ports::ButtonEdge;

/// Deep enough that a burst of edges can never block the task that produces
/// them: this task must always be back at the pin, both to catch the next edge
/// and to keep the deep-sleep wake source armed.
const EDGE_QUEUE_DEPTH: usize = 8;
pub type EdgeChannel = Channel<CriticalSectionRawMutex, ButtonEdge, EDGE_QUEUE_DEPTH>;
pub type EdgeSender<'a> = Sender<'a, CriticalSectionRawMutex, ButtonEdge, EDGE_QUEUE_DEPTH>;
pub type EdgeReceiver<'a> = Receiver<'a, CriticalSectionRawMutex, ButtonEdge, EDGE_QUEUE_DEPTH>;

/// Runs forever, publishing edges.
pub async fn run(mut button: Flex<'_>, edges: EdgeSender<'_>) -> ! {
    // Input with a pull-up: the pin is active low, and the pull is what holds it
    // at the level that does *not* wake the chip, so a floating pad cannot wake
    // it immediately and repeatedly.
    button.apply_input_config(&InputConfig::default().with_pull(Pull::Up));
    button.set_output_enable(false);
    button.set_input_enable(true);

    button
        .apply_wakeup_config(&WakeupConfig::default().with_low_power_path(true))
        .unwrap();

    loop {
        if button.is_high() {
            button.wait_for_falling_edge().await;
        }
        edges.send(ButtonEdge::Falling).await;

        button.wait_for_rising_edge().await;
        edges.send(ButtonEdge::Rising).await;
    }
}
