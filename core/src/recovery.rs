//! What a boot does about the boots before it that crashed.
//!
//! A boot that crashes in its startup - say, on a flash error the protocol
//! stack cannot get past - crashes again on the next try, and the panic
//! handler's reset turns that into a loop. The loop runs at full power and
//! reports nothing, so from outside it looks like a device that stopped, and
//! it empties the cell in hours.
//!
//! The device layer keeps the count of crashed boots in memory that survives a
//! reset; this module decides what the count means.

use crate::ports::WakeCause;
use crate::time::Millis;
use crate::tuning;

/// How a boot starts, given the boots before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Recovery {
    /// Sleep this long instead of starting, or `None` to start.
    pub back_off: Option<Millis>,
    /// The count to retain while this boot runs. A boot counts as crashed until
    /// it reaches a planned end, so a crash needs no code of its own to be
    /// counted; the device layer clears the count at every planned end.
    pub crashed_boots: u32,
    /// Whether the boot before this one crashed, and the boot should say so.
    pub after_crash: bool,
}

/// `crashed_boots` is how many boots in a row before this one crashed.
/// `host_reset` is a reset that the USB host asked for.
pub fn recover(crashed_boots: u32, wake: WakeCause, host_reset: bool) -> Recovery {
    // Flashing the firmware or attaching a monitor resets the chip over USB,
    // and the boot it cuts short never reaches a planned end. That is not a
    // crash. The count may also be left over from another firmware.
    if host_reset {
        return Recovery {
            back_off: None,
            crashed_boots: 1,
            after_crash: false,
        };
    }

    let after_crash = crashed_boots > 0;

    // A press means a person is there, and may be about to hold the button
    // for a factory reset - the cure for a store the stack cannot get past.
    if wake == WakeCause::Button || crashed_boots < tuning::CRASH_LOOP_BOOTS {
        return Recovery {
            back_off: None,
            crashed_boots: crashed_boots.saturating_add(1),
            after_crash,
        };
    }

    // One attempt after the back-off: if it crashes too, the count is back at
    // the limit and the boot after it backs off again.
    Recovery {
        back_off: Some(tuning::CRASH_LOOP_BACK_OFF),
        crashed_boots: tuning::CRASH_LOOP_BOOTS - 1,
        after_crash,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestCase {
        name: &'static str,
        input_crashed_boots: u32,
        input_wake: WakeCause,
        input_host_reset: bool,
        expected: Recovery,
    }

    #[test]
    fn test_recover() {
        let test_cases = vec![
            TestCase {
                name: "a boot after a planned end starts",
                input_crashed_boots: 0,
                input_wake: WakeCause::Timer,
                input_host_reset: false,
                expected: Recovery {
                    back_off: None,
                    crashed_boots: 1,
                    after_crash: false,
                },
            },
            TestCase {
                name: "a boot below the limit starts",
                input_crashed_boots: tuning::CRASH_LOOP_BOOTS - 1,
                input_wake: WakeCause::Cold,
                input_host_reset: false,
                expected: Recovery {
                    back_off: None,
                    crashed_boots: tuning::CRASH_LOOP_BOOTS,
                    after_crash: true,
                },
            },
            TestCase {
                name: "a boot at the limit backs off and allows one more attempt",
                input_crashed_boots: tuning::CRASH_LOOP_BOOTS,
                input_wake: WakeCause::Cold,
                input_host_reset: false,
                expected: Recovery {
                    back_off: Some(tuning::CRASH_LOOP_BACK_OFF),
                    crashed_boots: tuning::CRASH_LOOP_BOOTS - 1,
                    after_crash: true,
                },
            },
            TestCase {
                name: "a timer wake at the limit backs off too",
                input_crashed_boots: tuning::CRASH_LOOP_BOOTS,
                input_wake: WakeCause::Timer,
                input_host_reset: false,
                expected: Recovery {
                    back_off: Some(tuning::CRASH_LOOP_BACK_OFF),
                    crashed_boots: tuning::CRASH_LOOP_BOOTS - 1,
                    after_crash: true,
                },
            },
            TestCase {
                name: "a button wake always starts, so a factory reset stays possible",
                input_crashed_boots: tuning::CRASH_LOOP_BOOTS + 5,
                input_wake: WakeCause::Button,
                input_host_reset: false,
                expected: Recovery {
                    back_off: None,
                    crashed_boots: tuning::CRASH_LOOP_BOOTS + 6,
                    after_crash: true,
                },
            },
            TestCase {
                name: "a garbage count does not overflow",
                input_crashed_boots: u32::MAX,
                input_wake: WakeCause::Button,
                input_host_reset: false,
                expected: Recovery {
                    back_off: None,
                    crashed_boots: u32::MAX,
                    after_crash: true,
                },
            },
            TestCase {
                name: "a reset by the USB host starts fresh, whatever the count says",
                input_crashed_boots: 114_064,
                input_wake: WakeCause::Cold,
                input_host_reset: true,
                expected: Recovery {
                    back_off: None,
                    crashed_boots: 1,
                    after_crash: false,
                },
            },
        ];

        for TestCase {
            name,
            input_crashed_boots,
            input_wake,
            input_host_reset,
            expected,
        } in test_cases
        {
            let result = recover(input_crashed_boots, input_wake, input_host_reset);
            assert_eq!(result, expected, "Failed case: '{name}'");
        }
    }
}
