//! Where the decision layer's state actually lives.
//!
//! Two of the three lifetimes from `ARCHITECTURE.md` are implemented here:
//!
//! - **retained** - the panic flag and the crashed-boot count, in RTC RAM,
//!   which deep sleep keeps powered. A LIT device wakes by rebooting, so both
//!   would be lost in ordinary RAM.
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
use sequential_storage::cache::Cache;
use sequential_storage::map::{MapConfig, MapStorage};

use soil_sensor_core::ports::Calibration;
use soil_sensor_core::tuning::{SOIL_CAL_DEFAULT_DRY_MV, SOIL_CAL_DEFAULT_WET_MV};

use crate::flash::SharedFlash;

// --- Durable: the dry/wet calibration ---------------------------------------

const KEY_DRY_MV: u8 = 1;
const KEY_WET_MV: u8 = 2;

/// Reads the persisted references, falling back to the firmware defaults when
/// nothing has been calibrated yet.
///
/// The flash operation completes inside the poll of the task that started it:
/// see [`SharedKvStore`] for why nothing here may await across the flash.
pub fn load_calibration(flash: SharedFlash<'_, '_>, range: Range<u32>) -> Calibration {
    block_on(async {
        let mut buf = [0u8; 32];
        let mut storage = MapStorage::new(flash, MapConfig::new(range), Cache::new_uncached());

        let dry_mv = storage
            .fetch_item::<i32>(&mut buf, &KEY_DRY_MV)
            .await
            .ok()
            .flatten()
            .unwrap_or(SOIL_CAL_DEFAULT_DRY_MV);
        let wet_mv = storage
            .fetch_item::<i32>(&mut buf, &KEY_WET_MV)
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
    flash: SharedFlash<'_, '_>,
    range: Range<u32>,
    calibration: Calibration,
) -> bool {
    let mut buf = [0u8; 32];
    let mut storage = MapStorage::new(flash, MapConfig::new(range), Cache::new_uncached());

    if storage
        .store_item(&mut buf, &KEY_DRY_MV, &calibration.dry_mv)
        .await
        .is_err()
    {
        return false;
    }

    storage
        .store_item(&mut buf, &KEY_WET_MV, &calibration.wet_mv)
        .await
        .is_ok()
}

// --- Durable: the Matter key-value store -------------------------------------

type KvCell =
    BlockingMutex<NoopRawMutex, RefCell<SeqMapKvBlobStore<SharedFlash<'static, 'static>>>>;

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
        let cell: &'static KvCell = CELL
            .uninit()
            .init_with(BlockingMutex::new(RefCell::new(store)));
        Self { store: cell }
    }

    /// Runs one operation against the store.
    fn with<R>(
        &self,
        f: impl FnOnce(&mut SeqMapKvBlobStore<SharedFlash<'static, 'static>>) -> R,
    ) -> R {
        self.store.lock(|cell| {
            let mut store = cell
                .try_borrow_mut()
                .expect("the store is never re-entered: every operation completes inside one poll");
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

// --- Retained: the panic flag ------------------------------------------------

/// Identifies a panic flag this firmware wrote.
const PANIC_MAGIC: u32 = 0x5041_4e49; // "PANI"

/// The panic flag, plus the marker that says whether the RTC RAM holding it
/// actually contains one.
///
/// `#[ram(persistent)]` puts the storage in a `NOLOAD` section: the linker
/// neither initialises it nor zeroes it, so on a cold boot it holds whatever was
/// in RTC RAM. The magic is what separates "the previous boot left this" from
/// "this is garbage".
#[repr(C)]
struct PersistentPanic {
    magic: u32,
    panicked: u32,
}

// SAFETY: plain data with no invariants beyond `magic`.
unsafe impl esp_hal::Persistable for PersistentPanic {}

/// RTC RAM survives deep sleep and a software reset, and is never initialised;
/// see [`PersistentPanic`].
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

// --- Retained: the crashed-boot count ----------------------------------------

/// Identifies a crashed-boot count this firmware wrote.
const CRASHED_BOOTS_MAGIC: u32 = 0x4352_5348; // "CRSH"

/// How many boots in a row ended without a planned end, plus the marker that
/// says whether the RTC RAM holding it actually contains one. What the count
/// means is the decision layer's (`soil_sensor_core::recovery`).
#[repr(C)]
struct PersistentCrashedBoots {
    magic: u32,
    count: u32,
}

// SAFETY: plain data with no invariants beyond `magic`; the decision layer
// accepts any count.
unsafe impl esp_hal::Persistable for PersistentCrashedBoots {}

/// See [`PersistentPanic`] for why this is RTC RAM and never initialised.
#[ram(unstable(rtc_fast, persistent))]
static mut CRASHED_BOOTS: PersistentCrashedBoots = PersistentCrashedBoots { magic: 0, count: 0 };

/// The count the previous boot left behind. An RTC RAM image without the
/// magic is a power-on, which follows no crash.
pub fn crashed_boots() -> u32 {
    let crashed = unsafe { &*core::ptr::addr_of!(CRASHED_BOOTS) };
    if crashed.magic == CRASHED_BOOTS_MAGIC {
        crashed.count
    } else {
        0
    }
}

pub fn set_crashed_boots(count: u32) {
    let crashed = unsafe { &mut *core::ptr::addr_of_mut!(CRASHED_BOOTS) };
    crashed.count = count;
    crashed.magic = CRASHED_BOOTS_MAGIC;
}
