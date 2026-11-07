//! Shared access to the on-chip NOR flash.
//!
//! Two independent storage users need the flash: Matter's own
//! `SeqMapKvBlobStore` (fabric/session state, in the NVS partition) and
//! [`crate::store`]'s calibration (dry/wet references, in the `calib`
//! partition). The `FLASH` peripheral can only be taken once, and both users
//! live for the whole boot, so they share it through one mutex instead of
//! each claiming their own.
//!
//! The byte-level work is not ours: each operation constructs an
//! [`esp_bootloader_esp_idf::partitions::FlashRegion`] from the
//! `PartitionEntry` it targets, which bounds-checks against the partition,
//! refuses to touch the partition the running application lives in, and
//! bounces the byte slices into the word-aligned, DRAM-resident buffers the
//! `esp_hal::flash::Flash` driver needs. Addressing is partition-relative,
//! matching the region API: a caller's `0` is the first byte of its own
//! partition. This module was originally built on the deprecated
//! `esp-storage` driver, whose unguarded ROM operations failed outright
//! after the `esp-hal` fork's update - every persisted key read back as
//! `Error::StdIoError` on every boot.
//!
//! There is no read caching of our own. The KV store tracks its keys in RAM
//! (upstream rs-matter-embassy PR 91), so a lookup for a key that is not
//! present - of which the fabric table's loader does one per possible fabric
//! index at every startup, and fabric indices only ever grow - never touches
//! flash, and `sequential-storage`'s cache keeps page states and key
//! pointers in RAM after the first pass. What still walks the flash is that
//! first pass: one key scan per boot reading stored items' headers in small
//! sequential reads, a few tens of milliseconds of guarded ROM operations
//! (each masks the interrupts and suspends the flash cache for its
//! duration). A read-ahead window used to collapse that walk into a single
//! read; it was cut as not worth its weight here - the walk runs once per
//! boot, on a device whose reads are otherwise a handful, and the window's
//! own code was the bulk of this module.
//!
//! The bus is deliberately synchronous. The flash driver underneath is
//! blocking anyway (the ROM calls), and every operation completes inside
//! the poll of the task that started it - the discipline that keeps the
//! shared-flash users from deadlocking each other (see `fw/src/store.rs`).
//! An async wrapper would buy the ability to yield during long operations
//! at the price of per-call machinery, and there is little left to overlap.

use core::cell::RefCell;

use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embedded_storage_async::nor_flash::{ErrorType, MultiwriteNorFlash, NorFlash, ReadNorFlash};
use esp_bootloader_esp_idf::partitions::{Error, FlashRegion, PartitionEntry};
use esp_hal::flash::Flash;
use log::error;

/// The flash, under one lock: the two storage users' regions can never
/// overlap, because every flash operation completes inside one poll of one
/// task (see `fw/src/store.rs`).
pub struct FlashCell<'d> {
    flash: RefCell<Flash<'d, esp_hal::Blocking>>,
}

impl<'d> FlashCell<'d> {
    pub fn new(flash: Flash<'d, esp_hal::Blocking>) -> Self {
        Self {
            flash: RefCell::new(flash),
        }
    }

    /// Runs one operation against the region of `partition`.
    ///
    /// The region exists only for the closure's duration, which keeps the
    /// two partitions' regions from overlapping: nothing else can hold a
    /// region while this one runs.
    fn with_region<R>(
        &self,
        partition: PartitionEntry,
        f: impl FnOnce(&mut FlashRegion<'_, 'd>) -> Result<R, Error>,
    ) -> Result<R, Error> {
        let mut flash = self.flash.borrow_mut();
        match partition.as_flash_region(&mut flash) {
            Ok(mut region) => f(&mut region),
            // Only an encrypted partition lands here; the storage partitions
            // never are, and this firmware does not enable flash encryption.
            Err(e) => Err(e),
        }
    }

    /// `offset` is partition-relative, like the region API.
    fn read(&self, partition: PartitionEntry, offset: u32, bytes: &mut [u8]) -> Result<(), Error> {
        let result = self.with_region(partition, |region| region.read(offset, bytes));
        if let Err(e) = &result {
            error!(
                "flash read {:#x}+{} in {} failed: {:?}",
                offset,
                bytes.len(),
                partition.label_as_str(),
                e
            );
        }
        result
    }

    fn erase(&self, partition: PartitionEntry, from: u32, to: u32) -> Result<(), Error> {
        let result = self.with_region(partition, |region| region.erase(from, to));
        if let Err(e) = &result {
            error!(
                "flash erase {:#x}..{:#x} in {} failed: {:?}",
                from,
                to,
                partition.label_as_str(),
                e
            );
        }
        result
    }

    fn write(&self, partition: PartitionEntry, offset: u32, bytes: &[u8]) -> Result<(), Error> {
        let result = self.with_region(partition, |region| region.write(offset, bytes));
        if let Err(e) = &result {
            error!(
                "flash write {:#x}+{} in {} failed: {:?}",
                offset,
                bytes.len(),
                partition.label_as_str(),
                e
            );
        }
        result
    }
}

/// The flash, under one lock. A blocking mutex over `NoopRawMutex`: the locks
/// are never held across an await (there are no awaits in the flash path), so
/// on this single-core executor they are never contended, and a raw mutex
/// that masks interrupts would keep them off beyond what the driver already
/// does for each ROM operation.
pub type SharedFlashBus<'d> = BlockingMutex<NoopRawMutex, RefCell<FlashCell<'d>>>;

/// A `NorFlash` handle onto one partition of a [`SharedFlashBus`]. Cheap to
/// copy - a reference plus one `PartitionEntry` - so both storage consumers
/// can hold one.
///
/// Addressing is partition-relative: offset `0` is the first byte of
/// `partition`, matching the region API the operations run through.
#[derive(Clone, Copy)]
pub struct SharedFlash<'a, 'd> {
    bus: &'a SharedFlashBus<'d>,
    partition: PartitionEntry,
}

impl<'a, 'd> SharedFlash<'a, 'd> {
    /// A handle onto `partition` of `bus`.
    pub fn new(bus: &'a SharedFlashBus<'d>, partition: PartitionEntry) -> Self {
        Self { bus, partition }
    }

    /// The partition's length in bytes, in the relative addressing this
    /// handle and its callers use.
    pub fn partition_len(&self) -> u32 {
        self.partition.len()
    }
}

impl ErrorType for SharedFlash<'_, '_> {
    type Error = Error;
}

impl ReadNorFlash for SharedFlash<'_, '_> {
    const READ_SIZE: usize = 1;

    // The bodies below have no await points, on purpose: the operations are
    // synchronous down to the driver, so the await-less path sheds per-call
    // overhead for the reads that still happen. See the module docs.

    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.bus
            .lock(|cell| cell.borrow().read(self.partition, offset, bytes))
    }

    fn capacity(&self) -> usize {
        self.partition.len() as usize
    }
}

impl NorFlash for SharedFlash<'_, '_> {
    const WRITE_SIZE: usize = 4;
    const ERASE_SIZE: usize = 4096;

    async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.bus
            .lock(|cell| cell.borrow().erase(self.partition, from, to))
    }

    async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.bus
            .lock(|cell| cell.borrow().write(self.partition, offset, bytes))
    }
}

/// The ROM write primitive programs words onto an erased page without an
/// intervening erase, and so does the region this handle goes through.
impl MultiwriteNorFlash for SharedFlash<'_, '_> {}
