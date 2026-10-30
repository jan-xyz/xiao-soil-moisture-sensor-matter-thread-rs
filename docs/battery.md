# How the battery estimate works

The board has no current sensor and does not know what kind of AA cell is in
the holder. Everything here is an estimate from the resting voltage of one
reading. A fresh cell reads full. Towards the end, the estimate reads the cell
as emptier than it is, never fuller. A warning that comes early is a nuisance.
A warning that comes late is a dead sensor.

The estimate keeps no state. Each boot estimates from its own reading, so no
stored history, count or learned value can be wrong, stale or lost.

## The charge left, from the voltage

`BatPercentRemaining` comes from `tuning::BATTERY_CURVE`, a table of resting
voltage against charge left, with straight lines between the points.

No single curve fits every AA chemistry:

| Cell | Resting voltage over its life |
|---|---|
| Alkaline | falls steadily from about 1.6 V to 1.0 V |
| NiMH | starts near 1.35 V once rested, then sits flat near 1.25 V for most of its life |
| Lithium (Li-FeS2) | sits flat near 1.5 V, then drops fast at the very end |

One reading cannot tell the three apart, so the table is fitted to each end:

- **The top is optimistic.** It reads full from the voltage of a rested full
  NiMH cell, the lowest of the three. So every fresh cell reads full.
- **The end is pessimistic.** In the range where a controller warns about the
  battery, the table stays below the alkaline and NiMH curves. So the warning
  comes early.
- **The middle is a compromise.** Where a NiMH cell is still nearly full, an
  alkaline cell is about half empty. The table follows the NiMH cell there, so
  an alkaline cell reads fuller than it is in the middle of its life.

The points come from typical published curves, not from measurements of this
board.

The lithium cell is the case the voltage cannot handle: it reads as full until
shortly before it is empty.

The value is rounded down to whole percent.

## No cell

A reading below `tuning::BATTERY_PRESENT_MIN_MV` cannot come from an AA cell.
Even an exhausted one rests well above it. It is what the pin reads when the
holder is empty and the board runs from USB. The device then reports the
charge, the voltage and the time remaining as unknown (null), instead of an
empty cell that asks to be replaced.

## The time remaining

`BatTimeRemaining` is the charge left, times an assumed capacity, divided by
an assumed average draw (`soil_sensor_core::battery_time_remaining_s`). Both
assumptions are pessimistic:

- **Capacity** (`tuning::BATTERY_CAPACITY_MAH`): at the low end of what AA
  cells hold at this tiny drain.
- **Average draw** (`tuning::BATTERY_AVERAGE_UA`): one wake-up per idle
  period, with an awake time, an awake current and a sleep current that are
  each above the datasheet's figures and above what the simulator's wake-ups
  take.

So the time remaining is shorter than the life `make sim` models. A simulator
test checks that it stays shorter. The same margin covers the middle of an
alkaline cell's life, where the charge reads too high.

## What it does not know

- **The real draw and the real capacity.** Both are assumptions. Nothing on
  the board measures either.
- **How fast the cell is really draining.** One reading has no rate. A board
  that draws far more than assumed - like a radio receiver that never turns
  off - shows only as a falling charge, and the time remaining falls with it.
- **Short-idle operation.** A device that no controller has registered for
  check-ins never sleeps and draws far more than the assumed average. For it
  the time remaining is far too long.
- **A lithium cell's last days.** Its voltage stays high until the end.
