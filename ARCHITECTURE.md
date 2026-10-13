# Architecture

This document describes the intended *shape* of the firmware and the reasoning behind it.
It is deliberately abstract: it names layers and concepts, never files or symbols. Its job is to
answer one question - **where does this change belong?** - and a second one that follows from it:
**what would it cost to test?**

Nothing here is specific to one sensor, one board, one radio or one protocol version. It applies to
any device firmware that runs behaviour on an async embedded runtime while a protocol stack owns
state of its own.

## The problem this solves

Embedded firmware tends to grow one of two ways: everything in one loop, or a handful of concurrent
tasks sharing flags and channels. Both start cheap. Both end with behaviour that can only be
verified by flashing hardware and watching a serial log.

That is the wrong way round. The behaviour that matters - how often to report, when it is safe to
sleep, what a gesture means, when to give up - is the part most likely to be wrong and the part
least accessible to a test. The parts that are hard to test (silicon, radio timing, power domains)
are the parts least likely to be wrong, because they are borrowed from a vendor.

So: push everything physical to the outside, and treat behaviour as data.

## The layers

```
                 +-----------------------------------+
                 |         Composition root          |
                 |  the only place that knows all    |
                 +-----------------+-----------------+
                                   |
             +---------------------+--------------------+
             |                                          |
      +------v----------+                    +----------v--------+
      | Decision layer  |<-------------------| Integration layer |
      | what it does    |                    | how it speaks     |
      +------+----------+                    +----------+--------+
             |                                          |
             +--------------------+---------------------+
                                  | ports
                 +----------------+----------------+
                 |                                 |
          +------v-------+              +----------v-------+
          | Device layer |              | Simulation layer |
          | real silicon |              | scripted world   |
          +--------------+              +------------------+
```

| Layer | Owns | Must never know about |
|---|---|---|
| **Decision** | behaviour, policy, tuning values, the lifetime of every piece of state | hardware, radio, protocol, wall-clock time, concurrency |
| **Integration** | protocol and cluster semantics, network handling, the *mechanism* of persistence | hardware, and any user-facing policy |
| **Ports** | the capability vocabulary - the only thing the layers above and below share | anything concrete |
| **Device** | peripherals, pin mapping, the timing of the silicon | any decision |
| **Simulation** | a scripted world and a virtual clock | any decision |
| **Composition root** | wiring, concurrency, and the choice of device or simulation | any policy |

The arrows point one way. A layer may call the layer below it and may be called by the layer above
it, and never the reverse.

## The rules

1. **The decision layer is sequential and pure.** It consumes events and produces actions. It holds
   no handles, awaits nothing, and reads no clock. Everything it does is reproducible from the
   sequence of events it was given.
2. **Time is an input, not an ambient fact.** The decision layer is told what time it is, or asked
   to be woken at a deadline. It never asks.
3. **The world is concurrent; the decision layer is not.** Turning concurrent sources into one
   serial event stream, and actions back into concurrent effects, is the composition root's job.
   A source that must not lose events while another effect is running keeps a small buffer in the
   root, not in the decision layer.
4. **A port is a capability, not a device.** Ports are named after what the layer above needs
   ("read the ambient value"), not after what the layer below is ("read this ADC channel").
5. **The device layer contains no decisions and no thresholds.** If it needs to compare something,
   the comparison belongs one layer up.
6. **The simulation layer is a peer, not a test fixture.** The same code path runs against the real
   device layer and against the simulated one; only the composition root chooses between them.
7. **Every behaviour has exactly one implementation.** A second copy - in a test, in a mock, in a
   simulation - is a bug waiting to diverge.

## Where a change belongs

| Change | Layer(s) touched | Test that should exist |
|---|---|---|
| Report less often, raise a threshold, retune a cadence | Decision | decision-layer case |
| Add or change a user gesture | Decision | decision-layer case |
| Add a sensor on an existing bus | Ports, Device, Simulation | simulation scenario |
| Add a protocol attribute, command or event | Integration | protocol test against the stack |
| Adopt a protocol concept (a sleep mode, a reachability profile) | Integration for the mechanism, Decision for the policy | both |
| Support a different board or MCU | Device only | build; behaviour unchanged |
| Support a different radio or network stack | Integration and Device | build; radio timing is not simulable |
| Add a value that must outlive a restart | Decision (its lifetime), then Ports and both lower layers | simulation across restarts |
| Make a wake-up cheaper or faster | Device and tuning; validated by simulation, proven by measurement | simulation for the budget, hardware for the truth |
| Add a factory reset or a provisioning action | Decision (when), Integration (what to erase), Ports | decision case plus simulation |

If a change appears in more than two rows, it is probably two changes.

## State lifetimes

Every piece of state has one of three lifetimes, and the decision layer is where that is declared:

- **Volatile** - lost on any restart, including a low-power one. Most state.
- **Retained** - survives a restart that keeps the always-on domain powered; lost when power is
  removed. Suitable for derived values that are expensive to recompute but harmless to lose.
- **Durable** - survives everything, including power removal. The protocol's identity and
  registrations, user calibration, anything a person would notice losing.

Two consequences worth designing for explicitly:

- **A device that sleeps by restarting is not a special case.** It is the ordinary case with a
  shorter interval. Write the decision layer as a function of *one boot*, not of uptime, and let the
  root decide whether the next boot is a minute or a day away.
- **Anything derived must be recomputed on every boot**, and anything the protocol can re-derive
  should not be persisted at all. Persisting less is both cheaper and less likely to be wrong.

## Simulation

The simulation layer is what makes the rules above worth following. It provides a scripted world -
sensor readings, input events, protocol state changes - a clock that advances only when the code
asks it to, and storage for each of the three lifetimes above.

Because a boot is a function, the simulator can run many of them in sequence, carrying forward only
what the lifetimes say should survive. That turns the hardest questions in a battery device into
ordinary assertions: does the retained value come back, does the durable value stay, is the cadence
what we intended, how much of the time was the system awake.

Two things it cannot do, and it is worth being honest about both:

- It does not model the silicon. It cannot tell you whether a power domain actually powers down.
- It does not model radio or protocol timing. It cannot tell you whether a window is long enough for
  a real peer to answer.

Numbers that come out of it are a model, not a measurement.

## Getting there

The layers do not have to be built before the firmware works; they have to be extracted while it
keeps working.

1. Move the behaviour that is already pure - arithmetic and thresholds - down into the decision
   layer, and test it.
2. Take one state machine at a time out of the tasks that own it, express it as events and actions,
   and leave a thin adapter behind. The firmware's behaviour must not change in this step.
3. Introduce the ports and a single driver loop, and let the concurrent tasks collapse into it.
4. Build the simulation layer, and move the retained and durable state behind ports so it can model
   restarts.
5. Optionally, drive the integration layer against the real stack, in process. Be honest about
   what this buys: most of the code underneath is the stack's, so it pays off where the seam is
   fragile - a notification chain, a routing decision - and not where the answer is a constant
   somebody typed. Skip it if the seam is thin.

Each step is independently useful, and each one is reversible.

## How to tell the layering is healthy

- The decision layer's tests are instant and have no dependencies.
- Adding a peripheral touches one port and two implementations, and nothing else.
- Adding a feature does not add a task.
- The same driver loop runs against real and simulated ports.
- Reading the device layer tells you what the hardware does, never what the device does.

And the signals that it has drifted: a hardware type in the decision layer; a decision in the device
layer; a test that needs a real clock or a hardware-specific executor; two copies of the same
behaviour; a constant whose meaning depends on which layer is reading it.
