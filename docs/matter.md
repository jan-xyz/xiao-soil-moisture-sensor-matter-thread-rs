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
charge, resting voltage and an estimated time remaining. That last one is a
least-squares fit through the last twenty-four hours of resting voltage; it needs
four hours of samples before it will offer a number at all, because a shorter or
flatter trend is mostly converter noise.

`BatPercentRemaining` is in **half-percent units** — 0 to 200 for 0 to 100% —
which is a spec subtlety that is easy to get wrong and invisible until a
controller shows double the charge.

## The reporting deadband

Both handlers only report when a reading has moved by at least
`SOIL_REPORT_DELTA_PERCENT` (2) or `BATTERY_REPORT_DELTA_MV` (20) since the last
thing they reported. Below that, the reading is jitter, and reporting it would
wake the radio for nothing.

The comparison is against the last *reported* value rather than the previous
sample, so a slow trend accumulates until it crosses the threshold while
single-sample noise still does not get through.

The deadband only means anything *within* one boot. A deep sleep is a reboot, the
dataver starts again, and every subscribed attribute is re-reported on the next
wake-up. Suppressing that by persisting the last reported value would buy nothing.

## The device is a sleepy end device, and mostly asleep

The Thread role is a Sleepy End Device: the receiver is off between data polls and
the parent buffers anything addressed to it. On top of that, Matter defines two
profiles through the ICD Management cluster:

| | Short Idle Time (SIT) | Long Idle Time (LIT) |
|---|---|---|
| Slowest polling interval | 15 s | minutes |
| Reachable | any time, within 15 s | only after a Check-In |
| Controller support needed | none | the controller must register |
| Sleep model here | radio off, CPU awake | deep sleep, CPU off |

A LIT-capable device **operates as a SIT until a controller registers for
Check-Ins**, so it works with a controller that knows nothing about LIT — it just
runs the battery down at the SIT rate.

While operating as a LIT, the device deep-sleeps: CPUs, RAM and every APB-clocked
peripheral are powered off, and the wake-up is a reboot. That is only affordable
because nothing has to survive in RAM — the fabrics, the registrations, the CASE
resumption records, the subscriptions and the Thread attachment are all in flash,
and the battery trend is in RTC RAM.

Four conditions gate the sleep, all in `App::sleep_for`:

1. the device must be operating as a LIT;
2. the protocol stack must be idle;
3. no gesture may be in progress — a reboot would restart the gesture's timing,
   and the ten-second factory-reset hold would never complete;
4. the settle delay must have elapsed, so whatever the active period changed
   reaches flash first.

A boot that never becomes a LIT never sleeps at all. That is correct and it is
also the thing most likely to surprise you: with no controller registered, the
device sits awake at ~17 mA.

## The active window is the energy budget

Every wake-up is a reboot that then has to attach to Thread, re-register with SRP,
send its Check-In, let the controller re-subscribe and take its own reading — all
inside `tuning::ACTIVE_HOLD`, which is ten seconds, at ~20–28 mA.

That window is worth far more than the sleep it replaces. At fifteen-minute
wake-ups it dominates everything else:

| | Time | Current |
|---|---|---|
| Active window | 10 s per wake-up | ~17 mA (datasheet: modem-sleep, CPU idle, 160 MHz) |
| Deep sleep | the rest | ~7 µA (datasheet) |

Which works out at roughly 203 µA average and about **1.4 years on a 2500 mAh AA**
— and that number is a *model*. `make sim` computes it from the datasheet
currents and the duty cycle the simulation actually produced. It says what the
decisions cost, not what the board draws: the regulators, the flash and the radio
wake-up cost are all real and all absent from it. Only a power profiler settles
it.

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
sleeping for fifteen minutes at a time. The other fabric's controller — which
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
Thread SED: Idle / SIT, polling every 15000 ms     <- SIT, awake, ~17 mA
```

versus the LIT path proving itself:

```
ICD: Check-In to fabric 1 node ...
ICD idle: deep-sleeping for 899500 ms
... reboot ...
Woken up from deep sleep by the timer
```
