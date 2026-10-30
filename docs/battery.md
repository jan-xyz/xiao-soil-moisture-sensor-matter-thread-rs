# How the battery estimate works

The board has no current sensor and does not know what kind of AA cell is in
the holder. Everything here is an estimate from the resting voltage of one
reading. Every estimate is meant to be pessimistic: it reads the cell as emptier
than it is, never fuller. A warning that comes early is a nuisance. A warning
that comes late is a dead sensor.

The estimate keeps no state. Each boot estimates from its own reading, so no
stored history, count or learned value can be wrong, stale or lost.

## The charge left, from the voltage

`BatPercentRemaining` comes from `tuning::BATTERY_CURVE`, a table of resting
voltage against charge left, with straight lines between the points.

No single curve fits every AA chemistry:

| Cell | Resting voltage over its life |
|---|---|
| Alkaline | falls steadily from about 1.6 V to 1.0 V |
| NiMH | starts near 1.4 V, then sits flat near 1.25 V for most of its life |
| Lithium (Li-FeS2) | sits flat near 1.5 V, then drops fast at the very end |

The table stays at or below the alkaline and NiMH curves, so for those two it
reads too low, never too high. A full NiMH cell therefore reads about 60%. The
points come from typical published curves, not from measurements of this board.

The lithium cell is the case the voltage cannot handle: it reads as full until
shortly before it is empty.

The value is rounded down to whole percent.

## The time remaining

`BatTimeRemaining` is the charge left, times an assumed capacity, divided by
an assumed average draw (`soil_sensor_core::battery_time_remaining_s`). Both
assumptions are pessimistic:

- **Capacity: 2,000 mAh** (`tuning::BATTERY_CAPACITY_MAH`). Alkaline and NiMH
  AA cells hold 2,000-2,800 mAh at this tiny drain, a lithium cell about
  3,000 mAh.
- **Average draw: about 158 µA** (`tuning::BATTERY_AVERAGE_UA`). One wake-up
  per idle period, 5 s awake at 25 mA, the rest asleep at 20 µA. The datasheet
  puts an idle CPU with the radio off at 17 mA and deep sleep at 7 µA. The
  simulator's wake-ups last about 3.4 s.

A full cell therefore reads about 1.4 years. The simulator's energy model, with
the datasheet's currents and a 2,500 mAh cell, says about 4 years.

## What it does not know

- **The real draw and the real capacity.** Both are assumptions. A current
  measurement of this board would replace them.
- **How fast the cell is really draining.** One reading has no rate. A board
  that draws far more than assumed - like a radio receiver that never turns
  off - shows only as a falling charge, and the time remaining falls with it.
- **Short-idle operation.** A device that no controller has registered for
  check-ins never sleeps and draws far more than 158 µA. For it the time
  remaining is far too long.
- **A lithium cell's last days.** Its voltage stays high until the end.
