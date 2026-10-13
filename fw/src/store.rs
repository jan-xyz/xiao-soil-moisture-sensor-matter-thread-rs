//! Where the decision layer's state actually lives.
//!
//! Two of the three lifetimes from `ARCHITECTURE.md` are implemented here:
//!
//! - **retained** - the battery trend, in RTC RAM, which deep sleep keeps
//!   powered. A LIT device wakes by rebooting, so a trend in ordinary RAM would
//!   restart on every wake-up and could never accumulate the span an estimate
//!   needs.
//! - **durable** - the dry/wet calibration, in its own flash partition, so that
//!   a Matter factory reset does not force a recalibration and vice versa.
//!
//! The volatile lifetime needs no code: it is the boot's own stack.

use core::ops::Range;

use esp_hal::ram;
use sequential_storage::cache::NoCache;
use sequential_storage::map::{fetch_item, store_item};

use soil_sensor_core::math::DischargeHistory;
use soil_sensor_core::ports::{Calibration, Retained};
use soil_sensor_core::tuning::{
    BATTERY_HISTORY_INTERVAL_SECS, BATTERY_HISTORY_LEN, BATTERY_REPLACED_RISE_MV,
    SOIL_CAL_DEFAULT_DRY_MV, SOIL_CAL_DEFAULT_WET_MV,
};

use crate::flash::SharedFlash;

// --- Durable: the dry/wet calibration ---------------------------------------

const KEY_DRY_MV: u8 = 1;
const KEY_WET_MV: u8 = 2;

/// Reads the persisted references, falling back to the firmware defaults when
/// nothing has been calibrated yet.
pub async fn load_calibration(mut flash: SharedFlash<'_, '_>, range: Range<u32>) -> Calibration {
    let mut buf = [0u8; 32];
    let mut cache = NoCache::new();

    let dry_mv =
        fetch_item::<u8, i32, _>(&mut flash, range.clone(), &mut cache, &mut buf, &KEY_DRY_MV)
            .await
            .ok()
            .flatten()
            .unwrap_or(SOIL_CAL_DEFAULT_DRY_MV);
    let wet_mv =
        fetch_item::<u8, i32, _>(&mut flash, range.clone(), &mut cache, &mut buf, &KEY_WET_MV)
            .await
            .ok()
            .flatten()
            .unwrap_or(SOIL_CAL_DEFAULT_WET_MV);

    Calibration { dry_mv, wet_mv }
}

/// Stores both references. `false` when either write failed, in which case the
/// caller keeps using the previous calibration.
pub async fn store_calibration(
    mut flash: SharedFlash<'_, '_>,
    range: Range<u32>,
    calibration: Calibration,
) -> bool {
    let mut buf = [0u8; 32];
    let mut cache = NoCache::new();

    if store_item(
        &mut flash,
        range.clone(),
        &mut cache,
        &mut buf,
        &KEY_DRY_MV,
        &calibration.dry_mv,
    )
    .await
    .is_err()
    {
        return false;
    }

    store_item(
        &mut flash,
        range,
        &mut cache,
        &mut buf,
        &KEY_WET_MV,
        &calibration.wet_mv,
    )
    .await
    .is_ok()
}

// --- Retained: the battery trend --------------------------------------------

/// Identifies a trend this firmware wrote. Bump it whenever the layout changes,
/// so a stale RTC RAM image is discarded instead of being read as if it were
/// current.
const TREND_MAGIC: u32 = 0x534f_494c; // "SOIL"

/// The trend, plus the marker that says whether the RTC RAM holding it actually
/// contains one.
///
/// `#[ram(persistent)]` puts the storage in a `NOLOAD` section: the linker
/// neither initialises it nor zeroes it, so on a cold boot it holds whatever was
/// in RTC RAM. The magic is what separates "the previous wake-up left this" from
/// "this is garbage".
#[repr(C)]
struct PersistentTrend {
    magic: u32,
    history: DischargeHistory<{ BATTERY_HISTORY_LEN }>,
}

impl PersistentTrend {
    /// Never stored - the section is `NOLOAD` - but the initializer has to
    /// type-check. [`init_trend`] establishes the contents.
    const UNINIT: Self = Self {
        magic: 0,
        history: DischargeHistory::new(0, 0),
    };
}

// SAFETY: the struct is plain data with no invariants of its own. `magic` says
// whether `history` means anything, and `DischargeHistory` clamps its indices on
// use, so a torn or stale image degrades to a wrong estimate rather than to
// out-of-bounds access.
unsafe impl esp_hal::Persistable for PersistentTrend {}

/// RTC RAM survives deep sleep (the datasheet keeps the RTC timer and LP memory
/// powered), and deep sleep is a reboot, so this is the only way the trend
/// outlives a wake-up cycle.
#[ram(unstable(rtc_fast, persistent))]
static mut TREND: PersistentTrend = PersistentTrend::UNINIT;

/// The persistent storage itself.
fn persistent() -> &'static mut PersistentTrend {
    unsafe { &mut *core::ptr::addr_of_mut!(TREND) }
}

/// Restores the trend, starting a fresh one when the magic does not match (a
/// cold boot, or a first boot after a firmware change).
pub fn init_trend() {
    let trend = persistent();
    if trend.magic != TREND_MAGIC {
        trend.history =
            DischargeHistory::new(BATTERY_HISTORY_INTERVAL_SECS, BATTERY_REPLACED_RISE_MV);
        trend.magic = TREND_MAGIC;
    }
}

/// The trend storage.
///
/// Only [`init_trend`] (which runs before any task does) and the boot loop touch
/// it, and the boot loop is the only thing that reads the sensors - so there is
/// never a second accessor, let alone a concurrent one.
pub fn trend() -> &'static mut Retained {
    &mut persistent().history
}
