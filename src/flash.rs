//! Shared access to the on-chip NOR flash.
//!
//! `esp_storage::FlashStorage` owns the `FLASH` peripheral and can only be
//! constructed once, but two independent storage users need it: Matter's own
//! `SeqMapKvBlobStore` (fabric/session state) and [`crate::calibration`]
//! (dry/wet references), each over its own partition. Both borrow the same
//! instance through this mutex instead of each claiming their own.

use embassy_embedded_hal::adapter::BlockingAsync;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embedded_storage_async::nor_flash::{ErrorType, MultiwriteNorFlash, NorFlash, ReadNorFlash};
use esp_storage::{FlashStorage, FlashStorageError};

/// XIAO ESP32-C6 ships with 4 MB flash (see `partitions.csv` and the
/// original firmware's `CONFIG_ESPTOOLPY_FLASHSIZE_4MB`). `capacity()` on
/// the async trait is synchronous, so this is a fixed constant rather than
/// a locked read of the real device - it does not change at runtime.
const FLASH_CAPACITY: usize = 0x0040_0000;

pub type SharedFlashBus<'d> = Mutex<CriticalSectionRawMutex, BlockingAsync<FlashStorage<'d>>>;

/// A `NorFlash` handle onto a [`SharedFlashBus`]. Cheap to copy - just a
/// reference - so both storage consumers can hold one.
#[derive(Clone, Copy)]
pub struct SharedFlash<'a, 'd>(pub &'a SharedFlashBus<'d>);

impl ErrorType for SharedFlash<'_, '_> {
    type Error = FlashStorageError;
}

impl ReadNorFlash for SharedFlash<'_, '_> {
    const READ_SIZE: usize = FlashStorage::READ_SIZE;

    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.0.lock().await.read(offset, bytes).await
    }

    fn capacity(&self) -> usize {
        FLASH_CAPACITY
    }
}

impl NorFlash for SharedFlash<'_, '_> {
    const WRITE_SIZE: usize = FlashStorage::WRITE_SIZE;
    const ERASE_SIZE: usize = FlashStorage::ERASE_SIZE;

    async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.0.lock().await.erase(from, to).await
    }

    async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.0.lock().await.write(offset, bytes).await
    }
}

/// `esp_storage::FlashStorage` supports rewriting a page without an
/// intervening erase, and so does this wrapper.
impl MultiwriteNorFlash for SharedFlash<'_, '_> {}
