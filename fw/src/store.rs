//! Where the decision layer's state actually lives.
//!
//! Two of the three lifetimes from `ARCHITECTURE.md` are implemented here:
//!
//! - **retained** - the battery trend, in RTC RAM, which deep sleep keeps
//!   powered. A LIT device wakes by rebooting, so a trend in ordinary RAM would
//!   restart on every wake-up and could never accumulate the span an estimate
//!   needs.
//! - **durable** - the dry/wet calibration, in its own flash partition, so that
//!   a Matter factory reset does not force a recalibration and vice versa; and
//!   the Matter key-value store, in the NVS partition, shared between the boot
//!   loop and the protocol stack.
//!
//! The volatile lifetime needs no code: it is the boot's own stack.

use core::cell::RefCell;
use core::ops::Range;

use embassy_futures::block_on;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use esp_hal::ram;
use rs_matter_embassy::matter::error::Error;
use rs_matter_embassy::matter::persist::KvBlobStore;
use rs_matter_embassy::matter::utils::init::InitMaybeUninit;
use rs_matter_embassy::persist::SeqMapKvBlobStore;
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
///
/// The flash operation completes inside the poll of the task that started it:
/// see [`SharedKvStore`] for why nothing here may await across the flash.
pub fn load_calibration(mut flash: SharedFlash<'_, '_>, range: Range<u32>) -> Calibration {
    block_on(async {
        let mut buf = [0u8; 32];
        let mut cache = NoCache::new();

        let dry_mv = fetch_item::<u8, i32, _>(
            &mut flash,
            range.clone(),
            &mut cache,
            &mut buf,
            &KEY_DRY_MV,
        )
        .await
        .ok()
        .flatten()
        .unwrap_or(SOIL_CAL_DEFAULT_DRY_MV);
        let wet_mv = fetch_item::<u8, i32, _>(
            &mut flash,
            range,
            &mut cache,
            &mut buf,
            &KEY_WET_MV,
        )
        .await
        .ok()
        .flatten()
        .unwrap_or(SOIL_CAL_DEFAULT_WET_MV);

        Calibration { dry_mv, wet_mv }
    })
}

/// Stores both references. `false` when either write failed, in which case the
/// caller keeps using the previous calibration.
pub fn store_calibration(
    flash: SharedFlash<'_, '_>,
    range: Range<u32>,
    calibration: Calibration,
) -> bool {
    block_on(async { store_calibration_async(flash, range, calibration).await })
}

async fn store_calibration_async(
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

// --- Durable: the Matter key-value store -------------------------------------

type KvCell = BlockingMutex<
    NoopRawMutex,
    RefCell<SeqMapKvBlobStore<SharedFlash<'static, 'static>>>,
>;

/// The Matter key-value store, shared between the boot loop and the protocol
/// stack.
///
/// The boot loop runs while the stack is still starting up, so the store can
/// no longer be handed exclusively to whichever side asks first: the stack's
/// startup rehydrates through it, and the boot loop's reboot count persists
/// through it. A blocking mutex over a `RefCell` gives every user
/// the same serialised view.
///
/// The mutex is [`NoopRawMutex`] on purpose, and the discipline around it is
/// load-bearing: every flash operation - this store's, and the calibration
/// store's above - completes inside the poll of the task that started it, so
/// on this single-core executor two operations can never be in flight at
/// once, and the lock is never contended. A raw mutex that masks interrupts
/// would keep them off for the length of a flash erase, which the radio's
/// timing cannot spare; and a wait for the lock of an unpolled task cannot
/// ever complete, so any discipline that allowed one is a deadlock.
#[derive(Clone, Copy)]
pub struct SharedKvStore {
    store: &'static KvCell,
}

impl SharedKvStore {
    /// Parks `store` in a static and returns the handle every user shares.
    pub fn share(store: SeqMapKvBlobStore<SharedFlash<'static, 'static>>) -> Self {
        static CELL: static_cell::StaticCell<KvCell> = static_cell::StaticCell::new();
        let cell: &'static KvCell = CELL.uninit().init_with(BlockingMutex::new(RefCell::new(store)));
        Self { store: cell }
    }

    /// Runs one operation against the store.
    fn with<R>(
        &self,
        f: impl FnOnce(&mut SeqMapKvBlobStore<SharedFlash<'static, 'static>>) -> R,
    ) -> R {
        self.store.lock(|cell| {
            let mut store = cell.try_borrow_mut().expect(
                "the store is never re-entered: every operation completes inside one poll",
            );
            f(&mut store)
        })
    }
}

impl KvBlobStore for SharedKvStore {
    fn load<'a>(&mut self, key: u16, buf: &'a mut [u8]) -> Result<Option<&'a [u8]>, Error> {
        self.with(|store| store.load(key, buf))
    }

    fn store(&mut self, key: u16, data: &[u8], buf: &mut [u8]) -> Result<(), Error> {
        self.with(|store| store.store(key, data, buf))
    }

    fn remove(&mut self, key: u16, buf: &mut [u8]) -> Result<(), Error> {
        self.with(|store| store.remove(key, buf))
    }
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

// --- Retained: the panic flag ------------------------------------------------

/// Identifies a panic flag this firmware wrote.
const PANIC_MAGIC: u32 = 0x5041_4e49; // "PANI"

/// The panic flag, plus the marker that says whether the RTC RAM holding it
/// actually contains one. Lives apart from [`PersistentTrend`] so the trend's
/// layout and magic stay about the trend.
#[repr(C)]
struct PersistentPanic {
    magic: u32,
    panicked: u32,
}

// SAFETY: plain data with no invariants beyond `magic`.
unsafe impl esp_hal::Persistable for PersistentPanic {}

/// See [`PersistentTrend`] for why this is RTC RAM and never initialised.
#[ram(unstable(rtc_fast, persistent))]
static mut PANIC: PersistentPanic = PersistentPanic {
    magic: 0,
    panicked: 0,
};

/// Marks the current boot as the one that panicked. Called from the panic
/// handler, so this must not allocate, lock, log or panic: two volatile stores
/// into RTC RAM, magic last because it is the commit marker.
pub fn set_panic_flag() {
    let flag = unsafe { &mut *core::ptr::addr_of_mut!(PANIC) };
    unsafe {
        core::ptr::write_volatile(core::ptr::addr_of_mut!(flag.panicked), 1);
        core::ptr::write_volatile(core::ptr::addr_of_mut!(flag.magic), PANIC_MAGIC);
    }
}

/// Whether the previous boot panicked, consuming the flag. An RTC RAM image
/// without the magic is a cold boot, not a panic.
pub fn take_panic_flag() -> bool {
    let flag = unsafe { &mut *core::ptr::addr_of_mut!(PANIC) };
    let panicked = flag.magic == PANIC_MAGIC && flag.panicked != 0;
    flag.panicked = 0;
    panicked
}
