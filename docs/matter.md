# The Matter side, and what the radio costs

## Two clusters, both hand-written

`rs-matter` generates the raw attribute plumbing for every cluster in the Matter
IDL, including `SoilMeasurement` — but it ships no ergonomic handler for it, the
way it does for `OnOff` or `Descriptor`. Its built-in `PowerSourceHandler` is
hardcoded to the *wired* feature, so the battery attributes always answer
`AttributeNotFound`.

`node/` therefore hand-writes both handlers, following the same recipe
`DescHandler` uses: re-export the generated types, hold the endpoint id, a dataver
and a reference to a shared cell, and implement `ClusterHandler`.

The battery cluster claims the `BATTERY` feature, which makes `BatChargeLevel`,
`BatReplacementNeeded` and `BatReplaceability` mandatory, and reports state of
charge and resting voltage. How the charge is estimated is in
[`battery.md`](battery.md); `BatTimeRemaining` is not served, to keep the
report lean.

`BatPercentRemaining` is in **half-percent units** — 0 to 200 for 0 to 100% —
which is a spec subtlety that is easy to get wrong and invisible until a
controller shows double the charge.

## The reporting deadband

Both handlers only report when a reading has moved by at least
`SOIL_REPORT_DELTA_PERCENT` (2) or `BATTERY_REPORT_DELTA_MV` (20) since the last
thing they reported. Below that, the reading is jitter, and reporting it would
wake the radio for nothing.

The battery handler also reports when the charge moved by
`BATTERY_REPORT_DELTA_HALF_PERCENT` (10, so 5%) or the charge level changed. The
battery curve is steep between 1200 and 1300 mV, so a change inside the voltage
deadband can be more than ten percent and can cross the 20% warning threshold.
A report names all four attributes a reading sets: the percent, the voltage,
the charge level and the replacement flag.

The comparison is against the last *reported* value rather than the previous
sample, so a slow trend accumulates until it crosses the threshold while
single-sample noise still does not get through.

The deadband only means anything *within* one boot. A deep sleep is a reboot, the
dataver starts again, and every subscribed attribute is re-reported on the next
wake-up. Suppressing that by persisting the last reported value would buy nothing.

The sampling interval is constant, on the same reasoning. The reported percent
scale decays roughly linearly - the trip from 100% to 90% takes about as long as
the one from 10% to 0% - so a fixed interval samples the process at uniform
resolution. An interval that adapted to the reading (dry soil polled faster,
wet soil slept longer) would spend `IdleModeDuration` writes and
re-advertisement on information the linearity already gives, so it was rejected;
the tuning note in `core/src/tuning.rs` on `LIT_POLL_PERIOD_MS` carries the
energy argument that picked the number.

## The device is a sleepy end device, and mostly asleep

The Thread role is a Sleepy End Device: the receiver is off between data polls and
the parent buffers anything addressed to it. On top of that, Matter defines two
profiles through the ICD Management cluster:

| | Short Idle Time (SIT) | Long Idle Time (LIT) |
|---|---|---|
| Slowest polling interval | seconds | minutes |
| Reachable | any time, within one polling interval | only after a Check-In |
| Controller support needed | none | the controller must register |
| Sleep model here | radio off, CPU awake | deep sleep, CPU off |

A LIT-capable device **operates as a SIT until a controller registers for
Check-Ins**, so it works with a controller that knows nothing about LIT — it just
runs the battery down at the SIT rate.

While operating as a LIT, the device deep-sleeps: CPUs, RAM and every APB-clocked
peripheral are powered off, and the wake-up is a reboot. That is only affordable
because nothing has to survive in RAM — the fabrics, the registrations, the
subscriptions and the Thread attachment are all in flash, and the battery
estimate needs nothing from an earlier boot.

The CASE resumption records need one extra task. CASE is the handshake that
opens a secure session between the device and a controller. A resumption record
lets the next session skip most of that handshake. `rs-matter` loads the records
at startup, and its `Matter::run_persist_resumption` task writes them back after
every change. The firmware runs that task next to the protocol stack.

The task waits `tuning::RESUMPTION_PERSIST_DEBOUNCE` after a change before it
writes. Every message keeps the device awake for longer than that, so the last
change of a boot reaches flash before the deep sleep. A record that is lost or
unreadable costs one full handshake, and nothing else.

Four conditions gate the sleep, all in `App::sleep_for`:

1. the device must be operating as a LIT;
2. the protocol stack must be idle;
3. no gesture may be in progress — a reboot would restart the gesture's timing,
   and the factory-reset hold would never complete;
4. the settle delay must have elapsed, so whatever the active period changed
   reaches flash first.

A boot that never becomes a LIT never sleeps at all. That is correct and it is
also the thing most likely to surprise you: with no controller registered, the
device sits awake at the idle-CPU current.

## The active window is the energy budget

Every wake-up is a reboot that then has to attach to Thread, re-register with SRP,
send its Check-In, let the controller re-subscribe and take its own reading — all
inside the active window that `tuning::ACTIVE_HOLD` starts and every message
extends.

That window is worth far more than the sleep it replaces. Measured with a
multimeter in series with the cell, at the current tuning:

| | Time | Current (at the cell) |
|---|---|---|
| Active window | ~7.6 s of the cycle | ~100 mA |
| Deep sleep | the rest | ~0.12 mA |

The awake figure is battery-side and carries everything: the Thread radio and
the CPU, and the carrier's boost converter, which pulls ~100 mA from the 1.5 V
cell to deliver ~38 mA at 3.3 V. The datasheet's 17 mA "idle CPU" figure
understates a wake fourfold - the radios run with the CPU, and boosting from
1.5 V multiplies the cell-side current.

The deep-sleep figure is a carrier-board property, not a chip one: the chip's
share is the datasheet's ~7 µA, and the boost converter - which the firmware
cannot gate, see [`hardware.md`](hardware.md) - keeps the 3V3 rail alive
through the sleep for the rest.

One wake-up per idle period therefore cost ~0.54 mA of average current at the
old 30-minute idle; at the 6-hour one it is ~0.16 mA, because the deep-sleep
floor - not the wakes - is the budget now. That is over a year on an AA cell,
well past the stock Wi-Fi firmware's reported 4-5 months, and past the point
where stretching the idle further buys anything measurable: the next gain
lives in the carrier board's sleep current, not in the cadence.

`make sim` turns this into an average current and a battery life. Its currents
are the measured ones, and its per-wake time is the simulation's own accounting
plus a modelled share for the protocol stack, which the simulation does not
run. It says what the decisions cost and what the radio work around them is
assumed to cost. Only a power profiler settles the rest.

## The ICD Management cluster lives on endpoint 0

Endpoint 0 is the root and system endpoint, and the stack's `root_endpoint!` macro
cannot forward extra clusters — so `fw/src/main.rs` spells the endpoint out and
uses the `clusters!(thread ; ...)` form instead.

The ordering of the handler chain matters and is not obvious: `EmptyHandler::chain`
wraps LIFO, so the *last* chained matcher is evaluated first. The broad "any
cluster on endpoint 0" matcher has to be chained **first** so that it is evaluated
last; reverse them and it claims every endpoint-0 cluster, answering ICD
Management reads with `EndpointNotFound`.

Check-Ins, the `StayActiveRequest` deadline and the SIT/LIT switch all belong to
`rs-matter` (`LitIcd` and `LitIcdMgmtHandler`). There is no app-side Check-In task.

## Multiple fabrics: a hazard worth knowing about

`rs-matter` decides the operating mode like this:

```rust
if self.clients.is_empty() || self.sit_required {
    OperatingModeEnum::SIT
} else {
    OperatingModeEnum::LIT
}
```

`clients` is the flat list across **all** fabrics. So the moment *either*
controller registers for Check-Ins, the whole device flips to LIT and starts
sleeping for one idle period at a time. The other fabric's controller — which
never registered, and may not support LIT at all — loses reachability to the
device entirely.

If you pair this device with two ecosystems, be aware of that. The remedy would be
to require *every* fabric to have a registered client before going LIT, which is
what this codebase's documentation once asked for — but `LitIcd` does not expose
the per-fabric registration state, so it is not expressible from outside
`rs-matter` today. The only lever is `set_sit_required`, which is all-or-nothing.

## Controller support

LIT only became certifiable with Matter 1.4, and controller support has lagged
further. At the time of writing Home Assistant and SmartThings register for
Check-Ins; Google Home and Apple Home had implemented it internally but not
shipped it. On any of those, the device falls back to SIT and never sleeps.

The way to tell which mode you are in, from a serial log:

```
ICD: idle mode
Thread SED: Idle / SIT, polling every ... ms       <- SIT, awake
```

versus the LIT path proving itself:

```
ICD: Check-In to fabric 1 node ...
ICD idle: deep-sleeping for ...
... reboot ...
Woken up from deep sleep by the timer
```
