# How the battery estimate works

The board has no current sensor and does not know what kind of AA cell is in
the holder. Everything here is an estimate from the resting voltage. Every
estimate is meant to be pessimistic: it reads the cell as emptier than it is,
never fuller. A warning that comes early is a nuisance. A warning that comes
late is a dead sensor.

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
