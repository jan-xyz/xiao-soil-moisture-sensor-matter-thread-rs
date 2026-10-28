//! Shared access to the on-chip NOR flash.
//!
//! `esp_storage::FlashStorage` owns the `FLASH` peripheral and can only be
//! constructed once, but two independent storage users need it: Matter's own
//! `SeqMapKvBlobStore` (fabric/session state) and [`crate::store`]'s
//! calibration (dry/wet references), each over its own partition. Both borrow
//! the same instance through this mutex instead of each claiming their own.
//!
//! The bus is deliberately synchronous. The flash driver underneath is
//! blocking anyway (`esp_storage` calls the ROM), and every operation
//! completes inside the poll of the task that started it - the discipline
//! that keeps the shared-flash users from deadlocking each other (see
//! `fw/src/store.rs`). An async wrapper would buy the ability to yield
//! during long operations at the price of per-call machinery, and there is
//! little left to overlap: since the KV store tracks its keys in RAM
//! (upstream rs-matter-embassy PR 91), the fabric table's one-lookup-per-
//! possible-index probes - 254 of them per startup, since fabric indices
//! are assigned monotonically and are never reused - no longer reach flash
//! at all, and the reads that remain are a handful per boot.
//!
//! What the synchronous path gives up is the yield during long operations:
//! an erase blocks the executor for its duration (tens of ms, up to ~100
//! for a full sector). That is accepted, because the operations that erase
//! are rare: the calibration flow, the KV store's occasional page
//! compaction, and factory resets.
//!
//! On top sits a read-ahead window, sized past the largest partition the
//! firmware scans (the NVS spans 24 KiB, `partitions.csv`). The reads that
//! remain - the KV store's first key scan of a boot, the calibration
//! lookups - walk stored items' headers in small sequential reads, so the
//! first read of a walk fills the window once and every further read over
//! the same region is pure RAM. A one-page buffer was tried first and
//! taught the lesson this one encodes: a cache smaller than the working
//! set of a repeated scan helps nothing. The window is anchored at a
//! window-size boundary, so a partition that fits inside one window is
//! covered no matter where in it a scan reads first. It is invalidated on
//! every write and erase, and it lives under the same lock as the flash,
//! so a scanner can never hold a stale window across someone else's write.

use core::cell::RefCell;

use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embedded_storage_async::nor_flash::{ErrorType, MultiwriteNorFlash, NorFlash, ReadNorFlash};
use esp_storage::{FlashStorage, FlashStorageError};

/// XIAO ESP32-C6 ships with 4 MB flash (see `partitions.csv` and the
/// original firmware's `CONFIG_ESPTOOLPY_FLASHSIZE_4MB`). `capacity()` on
/// the async trait is synchronous, so this is a fixed constant rather than
/// a locked read of the real device - it does not change at runtime.
const FLASH_CAPACITY: u32 = 0x0040_0000;

/// The read-ahead window: sized past the largest partition the firmware
/// scans (the NVS spans 24 KiB, `partitions.csv`), so a scan fills the
/// buffer once and every further walk over the same region is pure RAM.
/// Reads that span more than the window bypass it rather than stitching
/// windows together; nothing in this firmware reads that much at once.
const READ_AHEAD_WINDOW: u32 = 0x8000;

/// One flash window held in RAM, so repeated scans over the same region
/// cost one real flash read.
struct ReadAhead {
    /// The flash offset `buf` holds, or `None` while it holds nothing.
    window: Option<u32>,
    buf: [u8; READ_AHEAD_WINDOW as usize],
}

impl ReadAhead {
    const fn new() -> Self {
        Self {
            window: None,
            buf: [0; READ_AHEAD_WINDOW as usize],
        }
    }

    /// Whether `[offset, offset + len)` lies inside the buffered window.
    fn covers(&self, offset: u32, len: usize) -> bool {
        match self.window {
            Some(window) => offset >= window && (offset - window) as usize + len <= self.buf.len(),
            None => false,
        }
    }

    fn serve(&self, offset: u32, bytes: &mut [u8]) {
        let from = (offset - self.window.expect("serve on a covered read")) as usize;
        bytes.copy_from_slice(&self.buf[from..from + bytes.len()]);
    }
}

/// The flash, plus its read-ahead window, under one lock: the window is only
/// ever touched while the flash itself is locked, so a write or an erase can
/// never invalidate it behind the scanner's back.
pub struct FlashCell<'d> {
    flash: RefCell<FlashStorage<'d>>,
    read_ahead: RefCell<ReadAhead>,
}

impl<'d> FlashCell<'d> {
    pub fn new(flash: FlashStorage<'d>) -> Self {
        Self {
            flash: RefCell::new(flash),
            read_ahead: RefCell::new(ReadAhead::new()),
        }
    }

    fn read(&self, offset: u32, bytes: &mut [u8]) -> Result<(), FlashStorageError> {
        if self.read_ahead.borrow().covers(offset, bytes.len()) {
            self.read_ahead.borrow().serve(offset, bytes);
            return Ok(());
        }

        // A read that fits the window is worth filling the window - anchored
        // at a window-size boundary, so that a partition which fits inside
        // one window is covered no matter where in it a scan happens to read
        // first. The NVS does (0x9000..0xf000 inside 0x8000..0x10000), and
        // that is what turns the KV store's first key scan of a boot into
        // one flash read: every read over it, from any page, hits the same
        // buffered window.
        if bytes.len() as u32 <= READ_AHEAD_WINDOW {
            let window = offset - (offset % READ_AHEAD_WINDOW);
            let len = READ_AHEAD_WINDOW.min(FLASH_CAPACITY - window) as usize;

            self.flash
                .borrow_mut()
                .read(window, &mut self.read_ahead.borrow_mut().buf[..len])?;
            self.read_ahead.borrow_mut().window = Some(window);
            self.read_ahead.borrow().serve(offset, bytes);

            return Ok(());
        }

        // Too big for the buffer: straight through, leaving whatever window
        // is buffered alone - it may still serve the small reads around this
        // one.
        self.flash.borrow_mut().read(offset, bytes)
    }

    fn erase(&self, from: u32, to: u32) -> Result<(), FlashStorageError> {
        self.read_ahead.borrow_mut().window = None;
        self.flash.borrow_mut().erase(from, to)
    }

    fn write(&self, offset: u32, bytes: &[u8]) -> Result<(), FlashStorageError> {
        self.read_ahead.borrow_mut().window = None;
        self.flash.borrow_mut().write(offset, bytes)
    }
}

/// The flash, plus its read-ahead window, under one lock. A blocking mutex
/// over `NoopRawMutex`: the locks are never held across an await (there are
/// no awaits in the flash path), so on this single-core executor they are
/// never contended, and a raw mutex that masks interrupts would keep them
/// off for the length of an erase - which the radio's timing cannot spare.
pub type SharedFlashBus<'d> = BlockingMutex<NoopRawMutex, RefCell<FlashCell<'d>>>;

/// A `NorFlash` handle onto a [`SharedFlashBus`]. Cheap to copy - just a
/// reference - so both storage consumers can hold one.
#[derive(Clone, Copy)]
pub struct SharedFlash<'a, 'd>(pub &'a SharedFlashBus<'d>);

impl ErrorType for SharedFlash<'_, '_> {
    type Error = FlashStorageError;
}

impl ReadNorFlash for SharedFlash<'_, '_> {
    const READ_SIZE: usize = FlashStorage::READ_SIZE;

    // The bodies below have no await points, on purpose: the operations are
    // synchronous down to the driver, so the await-less path sheds per-call
    // overhead for the reads that still happen. See the module docs.

    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.0.lock(|cell| cell.borrow().read(offset, bytes))
    }

    fn capacity(&self) -> usize {
        FLASH_CAPACITY as usize
    }
}

impl NorFlash for SharedFlash<'_, '_> {
    const WRITE_SIZE: usize = FlashStorage::WRITE_SIZE;
    const ERASE_SIZE: usize = FlashStorage::ERASE_SIZE;

    async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.0.lock(|cell| cell.borrow().erase(from, to))
    }

    async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.0.lock(|cell| cell.borrow().write(offset, bytes))
    }
}

/// `esp_storage::FlashStorage` supports rewriting a page without an
/// intervening erase, and so does this wrapper.
impl MultiwriteNorFlash for SharedFlash<'_, '_> {}
