# Tests, and the simulator

Everything runs on the host. None of it needs a board, a controller or a power
profiler, and all of it finishes in well under a second.

```sh
make test     # the three suites
make sim      # run the device's behaviour against a scripted world
```

## Three suites

| Suite | What it covers |
|---|---|
| `core` | the arithmetic, the button gesture machine, the calibration flow, and the decision to sleep |
| `node` | the reporting deadband of both cluster handlers |
| `sim` | whole boots: sampling cadence, gestures, persistence across reboots, the energy budget |

## The simulator is a peer, not a mock

`sim` implements the same `Ports` trait the device layer implements, over a
scripted world with a clock that moves only when the code asks it to. It then runs
the same `core::app::run_boot` the firmware runs.

That distinction matters. A hand-written fake of the boot loop would be a second
implementation of the behaviour, free to drift from the real one; a fake of the
*peripherals* cannot drift, because there is no behaviour in it to drift.

Because a boot is a function, the simulator runs many of them in sequence,
carrying forward only what the state lifetimes say should survive: the durable
store (flash) persists, everything else is rebuilt. Running thirty simulated days is a loop.

## Using it

```sh
make sim ARGS="--days 30"
```
```
30.0 simulated days | awake 2.71 h (0.377%) | asleep 717.4 h | average 71.0 uA | ~4.0 years on a 2500 mAh AA
2871 boots | 2871 soil reports | 2871 battery reports | 0 reboot-count writes
3400 ms awake per boot on average
battery estimate: 76% left, 402 days remaining
```

`--verbose` prints one line per boot, with the wake cause, how long it stayed
awake, and how it ended. `--sit-for N` makes it operate as a short-idle device for
N seconds before a controller registers, which is the mode where it never sleeps.

This is the fastest way to answer "what would this tuning change actually do"
without flashing anything. It is also the only way to see the energy consequences
of a change to the active window before you spend a week measuring them.

## What it cannot tell you

- **Nothing about the silicon.** It cannot say whether a power domain powers down,
  whether the radio really stops, or what the board draws.
- **Nothing about radio or protocol timing.** It does not model Thread, the parent
  buffering a sleepy end device's downlink, or whether the active window is long
  enough for a real controller to answer.
- **Nothing about the driver.** The mapping from a polling interval onto
  OpenThread's data-poll period, and the child-timeout sizing, are in
  `rs-matter-embassy` and are not exercised at all.

Its numbers are a model. Treat them as a way to compare two designs, not as a
measurement of either.

## What it has already caught

Two bugs in the boot loop, on the simulator's first run — both of the kind that
would have been diagnosed as "the sensor is broken" on hardware:

- The loop did one thing per pass and then waited, so a reading that spanned the
  moment the protocol stack went idle left the device sampling every thirty
  seconds forever instead of sleeping. **It would never have slept.**
- A deadline that had already passed could be handed back as something to wait
  for, which is a wait nothing can end.

And one in the calibration path, which the simulator made obvious by refusing to
believe a reading: the stored calibration was written to flash but never read
back, so every boot measured against the firmware's built-in references and a
probe whose dry value sat above them reported 0% forever.

## What is deliberately not tested

**A protocol-level harness.** Driving the node through the interaction model in
process — rs-matter's own `tests/common/e2e` pattern — was considered and
rejected. Most of the code underneath it would be rs-matter's state machine and
TLV layer; the parts of ours it reaches are the cluster metadata constants, the
handler-chain predicates and the ICD configuration, which is configuration rather
than behaviour. The apparatus is around 250 lines coupled to a pinned revision.

The one thing it would genuinely buy is whether `report()` pushes a report to a
subscriber, as opposed to merely moving the dataver. Those are two different
things, the chain between them is not obvious, and it is not covered. Revisit if
that chain is touched, or if the cluster metadata stops being a direct
transcription of the spec.

**Hardware behaviour.** Verified by building, flashing, and watching.
