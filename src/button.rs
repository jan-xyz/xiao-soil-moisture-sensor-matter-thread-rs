//! User button: single press, triple click, and 10 s long-press.
//!
//! Replaces `button.cpp`. The original drives this off the `iot_button`
//! component's interrupt/timer state machine; there is no equivalent
//! off-the-shelf debounce/click-counting crate in this stack, so this is a
//! small hand-rolled async state machine over GPIO edge waits, mirroring the
//! same three gestures:
//!
//!   single press  -> sample now + moisture status blink
//!   triple press  -> dry/wet calibration flow
//!   10 s hold     -> factory reset (red warning blink)

use embassy_futures::select::{select, Either};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Channel, Sender};
use embassy_time::Timer;
use esp_hal::gpio::Input;

use crate::pins::{BUTTON_DEBOUNCE, BUTTON_FACTORY_RESET_HOLD, BUTTON_TRIPLE_CLICK_WINDOW};

#[derive(Clone, Copy, Debug)]
pub enum ButtonEvent {
    SampleNow,
    Calibrate,
    FactoryReset,
}

const EVENT_QUEUE_DEPTH: usize = 4;
pub type ButtonChannel = Channel<CriticalSectionRawMutex, ButtonEvent, EVENT_QUEUE_DEPTH>;
pub type ButtonSender<'a> = Sender<'a, CriticalSectionRawMutex, ButtonEvent, EVENT_QUEUE_DEPTH>;

/// Runs forever, translating GPIO edges on `button` (active-low) into
/// [`ButtonEvent`]s on `events`.
pub async fn run(mut button: Input<'_>, events: ButtonSender<'_>) -> ! {
    loop {
        button.wait_for_falling_edge().await;
        Timer::after(BUTTON_DEBOUNCE).await;
        if button.is_high() {
            // Debounce glitch, not a real press.
            continue;
        }

        match select(
            button.wait_for_rising_edge(),
            Timer::after(BUTTON_FACTORY_RESET_HOLD),
        )
        .await
        {
            Either::First(()) => count_clicks(&mut button, events).await,
            Either::Second(()) => {
                events.send(ButtonEvent::FactoryReset).await;
                button.wait_for_rising_edge().await;
            }
        }
    }
}

/// Called right after the first release. Counts additional presses within
/// `BUTTON_TRIPLE_CLICK_WINDOW` of each other, then dispatches once the
/// window lapses with no further press.
async fn count_clicks(button: &mut Input<'_>, events: ButtonSender<'_>) {
    let mut clicks = 1u32;

    while let Either::First(()) = select(
        button.wait_for_falling_edge(),
        Timer::after(BUTTON_TRIPLE_CLICK_WINDOW),
    )
    .await
    {
        clicks += 1;
        button.wait_for_rising_edge().await;
    }

    match clicks {
        1 => events.send(ButtonEvent::SampleNow).await,
        3 => events.send(ButtonEvent::Calibrate).await,
        _ => {}
    }
}
