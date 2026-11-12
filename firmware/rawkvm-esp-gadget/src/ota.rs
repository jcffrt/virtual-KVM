//! Firmware update over the network: the new image goes to the OTA app
//! partition that is not running, then that partition becomes the boot one.
//! The USB port is the HID device, so this is how the board gets updated.

use alloc::{boxed::Box, format, string::String};

use esp_bootloader_esp_idf::{
    ota_updater::OtaUpdater,
    partitions::{FlashStorage, PARTITION_TABLE_MAX_LEN},
};

const SECTOR: usize = 4096;
/// First byte of an ESP app image.
const IMAGE_MAGIC: u8 = 0xE9;

pub struct Ota {
    flash: FlashStorage<'static>,
    table: Box<[u8; PARTITION_TABLE_MAX_LEN]>,
    /// The update in progress: size, bytes received, sector buffer.
    update: Option<Update>,
    update_mode: bool,
    unlocked: bool,
}

/// `begin` outside the update mode: the board restarts into it (WiFi only,
/// no USB, core 1 off). Writing the flash while core 1 ran the USB from the
/// flash hung the board, with esp-storage's auto-park and with core 1
/// parked by hand alike.
pub const RESTART: &str = "ota: restarting into the update mode, send again";

struct Update {
    size: u32,
    received: u32,
    written: u32,
    /// Word-aligned: the ROM reads it as `u32`s.
    sector: Box<[u32; SECTOR / 4]>,
    fill: usize,
    /// Flash address of the partition being written.
    base: u32,
}

impl Update {
    fn bytes(&mut self) -> &mut [u8] {
        // SAFETY: a `[u32; N]` is `4 * N` initialized bytes.
        unsafe { core::slice::from_raw_parts_mut(self.sector.as_mut_ptr().cast::<u8>(), SECTOR) }
    }
}

impl Ota {
    pub fn new(flash: esp_hal::peripherals::FLASH<'static>, update_mode: bool) -> Self {
        Self {
            // SAFETY: the flash is written only in the update mode, where
            // core 1 never starts.
            flash: unsafe { FlashStorage::new(flash).multicore_ignore() },
            table: Box::new([0; PARTITION_TABLE_MAX_LEN]),
            update: None,
            update_mode,
            unlocked: false,
        }
    }

    /// Starts an update of `size` bytes. Only in the update mode: see
    /// [`Begin::Restart`].
    pub fn begin(&mut self, size: u32) -> Result<String, String> {
        if !self.update_mode {
            return Err(RESTART.into());
        }
        if !self.unlocked {
            // The first flash write also unlocks the flash: a step of its own,
            // to tell a hang there from one in the erase.
            crate::diag::step(format_args!("ota: unlock"));
            let rc = critical_section::with(|_| unsafe { esp_rom_sys::rom::spiflash::esp_rom_spiflash_unlock() });
            if rc != 0 {
                return Err(format!("ota: unlock failed ({rc})"));
            }
            self.unlocked = true;
        }
        crate::diag::step(format_args!("ota: begin, reading the partition table"));
        let mut updater = OtaUpdater::new(&mut self.flash, &mut self.table)
            .map_err(|e| format!("no OTA partitions: {e:?}"))?;
        let (region, part) = updater
            .next_partition()
            .map_err(|e| format!("next partition: {e:?}"))?;
        if size == 0 || size as usize > region.capacity() {
            return Err(format!("image of {size} bytes does not fit in {}", region.capacity()));
        }
        drop(region);
        drop(updater);
        let table = esp_bootloader_esp_idf::partitions::read_partition_table(
            &mut self.flash,
            &mut self.table[..],
        )
        .map_err(|e| format!("partition table: {e:?}"))?;
        let base = table
            .find_partition(esp_bootloader_esp_idf::partitions::PartitionType::App(part))
            .map_err(|e| format!("partition table: {e:?}"))?
            .ok_or_else(|| format!("no partition {part:?}"))?
            .offset();
        crate::diag::step_done();
        self.update = Some(Update {
            size,
            received: 0,
            written: 0,
            sector: Box::new([u32::MAX; SECTOR / 4]),
            fill: 0,
            base,
        });
        Ok(format!("ota: {size} bytes to {part:?} at {base:#x}"))
    }

    /// Adds image bytes; whole sectors go to flash.
    pub fn data(&mut self, mut bytes: &[u8]) -> Result<(), String> {
        let Some(u) = self.update.as_mut() else {
            return Err("ota: no update in progress".into());
        };
        if u.received == 0 && bytes.first() != Some(&IMAGE_MAGIC) {
            self.update = None;
            return Err("ota: not an ESP app image".into());
        }
        if u.received as usize + bytes.len() > u.size as usize {
            self.update = None;
            return Err("ota: more bytes than announced".into());
        }
        u.received += bytes.len() as u32;
        while !bytes.is_empty() {
            let n = (SECTOR - u.fill).min(bytes.len());
            let fill = u.fill;
            u.bytes()[fill..fill + n].copy_from_slice(&bytes[..n]);
            u.fill += n;
            bytes = &bytes[n..];
            if u.fill == SECTOR {
                if let Err(e) = flush_sector(u) {
                    self.update = None;
                    return Err(e);
                }
            }
        }
        Ok(())
    }

    /// Writes the last sector and makes the new image the boot one.
    pub fn end(&mut self) -> Result<String, String> {
        let (size, received, fill) = match self.update.as_ref() {
            Some(u) => (u.size, u.received, u.fill),
            None => return Err("ota: no update in progress".into()),
        };
        if received != size {
            self.update = None;
            return Err(format!("ota: got {received} of {size} bytes"));
        }
        if fill > 0 {
            let u = self.update.as_mut().expect("update in progress");
            if let Err(e) = flush_sector(u) {
                self.update = None;
                return Err(e);
            }
        }
        self.update = None;
        let mut updater = OtaUpdater::new(&mut self.flash, &mut self.table)
            .map_err(|e| format!("ota: {e:?}"))?;
        updater
            .activate_next_partition()
            .map_err(|e| format!("ota: activate: {e:?}"))?;
        crate::diag::step_done();
        Ok(format!("ota: {size} bytes written, restarting"))
    }
}

/// Erases and writes the buffered sector at the next offset of the
/// partition being written, with the ROM functions directly: esp-storage's
/// write hung the board (a 4 KiB buffer on the caller's stack, and no way to
/// tell where it stopped). A note before each step tells where a hang hit.
fn flush_sector(u: &mut Update) -> Result<(), String> {
    use esp_rom_sys::rom::spiflash::{esp_rom_spiflash_erase_sector, esp_rom_spiflash_write};
    let addr = u.base + u.written;
    crate::diag::step(format_args!("ota: erase at {addr:#x}"));
    let rc = critical_section::with(|_| unsafe { esp_rom_spiflash_erase_sector(addr / SECTOR as u32) });
    if rc != 0 {
        return Err(format!("ota: erase at {addr:#x}: {rc}"));
    }
    const PAGE: usize = 256;
    for page in 0..SECTOR / PAGE {
        let at = addr + (page * PAGE) as u32;
        let ptr = unsafe { u.sector.as_ptr().add(page * PAGE / 4) };
        crate::diag::step(format_args!("ota: write at {at:#x} from {ptr:p}"));
        let rc = critical_section::with(|_| unsafe { esp_rom_spiflash_write(at, ptr, PAGE as u32) });
        if rc != 0 {
            return Err(format!("ota: write at {at:#x}: {rc}"));
        }
    }
    u.written += SECTOR as u32;
    u.sector.fill(u32::MAX);
    u.fill = 0;
    Ok(())
}
