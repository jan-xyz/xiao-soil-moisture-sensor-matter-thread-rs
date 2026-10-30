//! The button gesture state machine: edges in, gestures out.
//!
//! Three gestures, all of them timing-based, all of them things a user will
//! notice if they are wrong:
//!
//!   single press  -> sample now, and say how wet it is
//!   triple press  -> start the dry/wet calibration flow
//!   long hold     -> factory reset
//!
//! The state machine is driven entirely by edges and by the passing of its own
//! deadlines, so it can be walked through a whole gesture in a test without a
//! pin, a timer or an executor.
//!
//! One case is worth calling out because it only exists on a device that sleeps:
//! when the button is what woke the device, the pin is *already* down when the
//! firmware starts, and the falling edge that would begin a press has already
//! happened. [`ButtonFsm::new`] therefore starts mid-gesture, which is what
//! makes the factory-reset hold work at all in the long-idle profile.

use crate::ports::ButtonEdge;
use crate::time::Instant;
use crate::tuning;

/// What the user asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonEvent {
    SampleNow,
    Calibrate,
    FactoryReset,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// Waiting for a press. Nothing is happening.
    Idle,
    /// The pin went low; waiting out the debounce before believing it.
    Debounce,
    /// Believing it. Waiting for the release, or for the hold to time out.
    Held,
    /// A factory reset has been asked for; waiting for the release so the
    /// gesture cannot fire twice.
    WaitRelease,
    /// Between presses, counting further ones.
    Window,
}

pub struct ButtonFsm {
    state: State,
    /// When the current press began.
    pressed_at: Instant,
    /// The deadline the current state is waiting for.
    deadline: Instant,
    /// Presses counted in the current gesture.
    clicks: u32,
    /// Set while the press that woke the device is still waiting to say what
    /// it wants. A boot orders its reading behind it, so the gesture's single
    /// click becomes the boot's one reading instead of a second one.
    waking: bool,
}

impl ButtonFsm {
    /// `held_at_boot` is true when this boot was caused by a button press, in
    /// which case the press is already in progress.
    pub fn new(held_at_boot: bool, now: Instant) -> Self {
        let mut fsm = Self {
            state: State::Idle,
            pressed_at: now,
            deadline: now,
            clicks: 0,
            waking: held_at_boot,
        };

        if held_at_boot {
            fsm.clicks = 1;
            fsm.pressed_at = now;
            fsm.state = State::Held;
            fsm.deadline = now.saturating_add(tuning::BUTTON_FACTORY_RESET_HOLD);
        }

        fsm
    }

    /// Feed an edge. Returns a gesture when one completes.
    pub fn on_edge(&mut self, edge: ButtonEdge, now: Instant) -> Option<ButtonEvent> {
        match (self.state, edge) {
            (State::Idle, ButtonEdge::Falling) => {
                self.clicks = 1;
                self.begin_press(now);
                None
            }
            (State::Window, ButtonEdge::Falling) => {
                self.clicks += 1;
                self.begin_press(now);
                None
            }
            // The pin went back up before the debounce elapsed: a contact
            // glitch, not a gesture - and the press that woke the device is
            // over all the same, so the boot may get on with its reading.
            (State::Debounce, ButtonEdge::Rising) => {
                self.waking = false;
                self.state = State::Idle;
                None
            }
            (State::Held, ButtonEdge::Rising) => {
                self.state = State::Window;
                self.deadline = now.saturating_add(tuning::BUTTON_TRIPLE_CLICK_WINDOW);
                None
            }
            (State::WaitRelease, ButtonEdge::Rising) => {
                self.state = State::Idle;
                None
            }
            _ => None,
        }
    }

    /// Advance time. Returns a gesture when one completes.
    pub fn on_deadline(&mut self, now: Instant) -> Option<ButtonEvent> {
        if now < self.deadline {
            return None;
        }

        match self.state {
            State::Debounce => {
                self.state = State::Held;
                self.deadline = now.saturating_add(tuning::BUTTON_FACTORY_RESET_HOLD);
                None
            }
            State::Held => {
                self.waking = false;
                self.state = State::WaitRelease;
                Some(ButtonEvent::FactoryReset)
            }
            State::Window => {
                self.waking = false;
                self.state = State::Idle;
                match self.clicks {
                    1 => Some(ButtonEvent::SampleNow),
                    3 => Some(ButtonEvent::Calibrate),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// When this machine next has something to do, if it does.
    pub fn next_deadline(&self) -> Option<Instant> {
        match self.state {
            State::Debounce | State::Held | State::Window => Some(self.deadline),
            State::Idle | State::WaitRelease => None,
        }
    }

    /// Whether a gesture is in progress. A boot must not end while this is
    /// true: a reboot would restart the gesture's timing, and the factory-reset
    /// hold would never complete.
    pub fn is_active(&self) -> bool {
        self.state != State::Idle
    }

    /// Whether the press that woke the device is still waiting to say what it
    /// wants. Every state this is true in waits on a deadline, so a boot that
    /// pumps the machine until this is false always terminates.
    pub(crate) fn waking_press_pending(&self) -> bool {
        self.waking && self.next_deadline().is_some()
    }

    fn begin_press(&mut self, now: Instant) {
        self.pressed_at = now;
        self.state = State::Debounce;
        self.deadline = now.saturating_add(tuning::BUTTON_DEBOUNCE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::Millis;

    /// An instant `ms` milliseconds after power-on.
    fn at(ms: u64) -> Instant {
        Instant::from_millis(ms)
    }

    const T0: Instant = Instant::ZERO;

    /// Press and release, leaving the click window to lapse. Returns the
    /// gesture, if any.
    fn tap(fsm: &mut ButtonFsm, pressed_at: Instant) -> Option<ButtonEvent> {
        assert_eq!(fsm.on_edge(ButtonEdge::Falling, pressed_at), None);

        let settled = pressed_at + tuning::BUTTON_DEBOUNCE;
        assert_eq!(
            fsm.on_deadline(settled),
            None,
            "a press on its own is not a gesture"
        );

        let released = settled + Millis::from_millis(50);
        assert_eq!(fsm.on_edge(ButtonEdge::Rising, released), None);

        fsm.on_deadline(released + tuning::BUTTON_TRIPLE_CLICK_WINDOW)
    }

    #[test]
    fn a_single_press_asks_for_a_sample() {
        let mut fsm = ButtonFsm::new(false, T0);
        assert_eq!(tap(&mut fsm, T0), Some(ButtonEvent::SampleNow));
        assert!(!fsm.is_active());
        assert_eq!(fsm.next_deadline(), None);
    }

    #[test]
    fn three_presses_ask_for_a_calibration() {
        let mut fsm = ButtonFsm::new(false, T0);
        let mut now = T0;

        // Two taps that stay inside each other's click windows.
        for _ in 0..2 {
            assert_eq!(fsm.on_edge(ButtonEdge::Falling, now), None);
            now = now + tuning::BUTTON_DEBOUNCE;
            assert_eq!(fsm.on_deadline(now), None);
            now = now + Millis::from_millis(50);
            assert_eq!(fsm.on_edge(ButtonEdge::Rising, now), None);
            now = now + Millis::from_millis(tuning::BUTTON_TRIPLE_CLICK_WINDOW.as_millis() / 2);
            assert_eq!(
                fsm.on_deadline(now),
                None,
                "still inside the window, so nothing is decided yet"
            );
        }

        assert_eq!(fsm.on_edge(ButtonEdge::Falling, now), None);
        now = now + tuning::BUTTON_DEBOUNCE;
        assert_eq!(fsm.on_deadline(now), None);
        now = now + Millis::from_millis(50);
        assert_eq!(fsm.on_edge(ButtonEdge::Rising, now), None);
        assert_eq!(
            fsm.on_deadline(now + tuning::BUTTON_TRIPLE_CLICK_WINDOW),
            Some(ButtonEvent::Calibrate)
        );
    }

    #[test]
    fn two_presses_are_not_a_gesture() {
        let mut fsm = ButtonFsm::new(false, T0);

        assert_eq!(fsm.on_edge(ButtonEdge::Falling, T0), None);
        assert_eq!(fsm.on_deadline(T0 + tuning::BUTTON_DEBOUNCE), None);
        let released = T0 + tuning::BUTTON_DEBOUNCE + Millis::from_millis(50);
        assert_eq!(fsm.on_edge(ButtonEdge::Rising, released), None);

        assert_eq!(fsm.on_edge(ButtonEdge::Falling, released), None);
        assert_eq!(fsm.on_deadline(released + tuning::BUTTON_DEBOUNCE), None);
        let released_again = released + tuning::BUTTON_DEBOUNCE + Millis::from_millis(50);
        assert_eq!(fsm.on_edge(ButtonEdge::Rising, released_again), None);

        assert_eq!(
            fsm.on_deadline(released_again + tuning::BUTTON_TRIPLE_CLICK_WINDOW),
            None,
            "two presses are neither a sample nor a calibration"
        );
    }

    #[test]
    fn a_release_before_the_debounce_is_a_glitch() {
        let mut fsm = ButtonFsm::new(false, T0);
        assert_eq!(fsm.on_edge(ButtonEdge::Falling, T0), None);
        assert!(fsm.is_active(), "a press in progress keeps the boot awake");

        assert_eq!(fsm.on_edge(ButtonEdge::Rising, at(5)), None);
        assert!(!fsm.is_active());
        assert_eq!(fsm.next_deadline(), None);

        assert_eq!(
            fsm.on_deadline(at(5) + tuning::BUTTON_TRIPLE_CLICK_WINDOW),
            None,
            "a glitch is not a gesture"
        );
    }

    #[test]
    fn holding_for_ten_seconds_asks_for_a_factory_reset() {
        let mut fsm = ButtonFsm::new(false, T0);
        assert_eq!(fsm.on_edge(ButtonEdge::Falling, T0), None);

        let settled = T0 + tuning::BUTTON_DEBOUNCE;
        assert_eq!(fsm.on_deadline(settled), None);

        let hold = settled + tuning::BUTTON_FACTORY_RESET_HOLD;
        assert_eq!(
            fsm.on_deadline(hold.saturating_sub(Millis::from_millis(1))),
            None,
            "not held long enough yet"
        );
        assert!(fsm.is_active());

        assert_eq!(fsm.on_deadline(hold), Some(ButtonEvent::FactoryReset));

        assert_eq!(
            fsm.on_deadline(hold + tuning::BUTTON_FACTORY_RESET_HOLD),
            None,
            "the reset is asked for once, not once per poll"
        );
        assert!(fsm.is_active(), "still held, so still not safe to sleep");

        assert_eq!(fsm.on_edge(ButtonEdge::Rising, hold), None);
        assert!(!fsm.is_active());
    }

    #[test]
    fn a_boot_caused_by_the_button_starts_mid_press() {
        // The press that woke the device is still going on, and no falling edge
        // is coming for it.
        let mut fsm = ButtonFsm::new(true, T0);
        assert!(fsm.is_active());
        assert_eq!(
            fsm.next_deadline(),
            Some(T0 + tuning::BUTTON_FACTORY_RESET_HOLD)
        );
        assert_eq!(
            fsm.on_deadline(T0 + tuning::BUTTON_FACTORY_RESET_HOLD),
            Some(ButtonEvent::FactoryReset)
        );
    }

    #[test]
    fn a_boot_caused_by_the_button_still_counts_a_tap() {
        // Woken by a press and released quickly: that is a single press, not a
        // reset, and the click window then applies as usual.
        let mut fsm = ButtonFsm::new(true, T0);
        let released = at(120);
        assert_eq!(fsm.on_edge(ButtonEdge::Rising, released), None);
        assert_eq!(
            fsm.on_deadline(released + tuning::BUTTON_TRIPLE_CLICK_WINDOW),
            Some(ButtonEvent::SampleNow)
        );
    }

    #[test]
    fn the_window_between_presses_is_what_counts_them() {
        let mut fsm = ButtonFsm::new(false, T0);
        assert_eq!(fsm.on_edge(ButtonEdge::Falling, T0), None);
        assert_eq!(fsm.on_deadline(T0 + tuning::BUTTON_DEBOUNCE), None);
        let released = T0 + tuning::BUTTON_DEBOUNCE + Millis::from_millis(50);
        assert_eq!(fsm.on_edge(ButtonEdge::Rising, released), None);

        // Too late: the window lapsed, so this was a single press.
        let late = released + tuning::BUTTON_TRIPLE_CLICK_WINDOW + Millis::from_millis(1);
        assert_eq!(fsm.on_deadline(late), Some(ButtonEvent::SampleNow));

        // And the machine is ready for the next gesture.
        assert_eq!(fsm.on_edge(ButtonEdge::Falling, late), None);
        assert_eq!(fsm.on_deadline(late + tuning::BUTTON_DEBOUNCE), None);
        assert_eq!(
            fsm.on_deadline(late + tuning::BUTTON_DEBOUNCE + tuning::BUTTON_FACTORY_RESET_HOLD),
            Some(ButtonEvent::FactoryReset)
        );
    }
}
