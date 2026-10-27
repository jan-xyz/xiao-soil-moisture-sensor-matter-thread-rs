//! The user button, as a source of edges.
//!
//! Deliberately the thinnest possible task: it owns the pin, keeps it listening,
//! and turns edges into messages. Everything that decides what a gesture *means*
//! is in the decision layer, where it can be tested.
//!
//! Two details are load-bearing:
//!
//! - the pin is configured and its task spawned at the very start of the boot
//!   (see `main`), before the Matter stack takes its seconds to come up, so
//!   that no edge of the press that woke the device can go unseen. The task
//!   keeps the pin on the chip's low-power pad path, because deep sleep
//!   powers down the GPIO peripheral and a wake source has to survive that.
//!   Only GPIO0..7 have such a path, which is why the button is not on the
//!   module's BOOT pin.
//! - a deep-sleep wake is a reboot that the button caused, so the chip comes
//!   back up with the button already down and no falling edge pending. The
//!   first thing the loop does is emit an edge for the press that is already
//!   happening.

use embassy_executor::task;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Channel, Receiver, Sender};
use esp_hal::gpio::{Flex, WakeupConfig};

use soil_sensor_core::ports::ButtonEdge;

/// Deep enough that a burst of edges can never block the task that produces
/// them: this task must always be back at the pin, both to catch the next edge
/// and to keep the deep-sleep wake source armed.
const EDGE_QUEUE_DEPTH: usize = 8;
pub type EdgeChannel = Channel<CriticalSectionRawMutex, ButtonEdge, EDGE_QUEUE_DEPTH>;
pub type EdgeSender<'a> = Sender<'a, CriticalSectionRawMutex, ButtonEdge, EDGE_QUEUE_DEPTH>;
pub type EdgeReceiver<'a> = Receiver<'a, CriticalSectionRawMutex, ButtonEdge, EDGE_QUEUE_DEPTH>;

/// Runs forever, publishing edges. The pin's input configuration was done by
/// `main` before this task was spawned, so that the level could be read for
/// the boot's own bookkeeping.
#[task]
pub async fn run(mut button: Flex<'static>, edges: EdgeSender<'static>) -> ! {
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
