//! The gadget the board presents, kept across resets.
//!
//! The USB device is built once at boot from the hub's last `Gadget`. A new
//! one is staged in RTC memory and the board restarts; at that boot, before
//! core 1 starts the USB, it is also written to flash so it survives a
//! power cut. Writing the flash while core 1 runs from it hangs the board
//! (see ota.rs), hence the restart.

use alloc::boxed::Box;

use light::Gadget;

const CAP: usize = 2048;
const MAGIC: u32 = 0x4741_4447; // "GDAG"
/// Last sector of the `nvs` partition, which nothing else uses.
const FLASH_ADDR: u32 = 0xC000;
const SECTOR: usize = 4096;
const HEADER: usize = 12;

#[repr(C)]
#[derive(Clone, Copy)]
struct Staged {
    magic: u32,
    len: u32,
    sum: u32,
    /// Not in flash yet.
    dirty: u32,
    data: [u8; CAP],
}

#[esp_hal::ram(unstable(rtc_fast, persistent))]
static mut STAGED: Staged = Staged {
    magic: 0,
    len: 0,
    sum: 0,
    dirty: 0,
    data: [0; CAP],
};

// SAFETY: integers and a byte array, any bit pattern is valid.
unsafe impl esp_hal::Persistable for Staged {}

fn checksum(b: &[u8]) -> u32 {
    // FNV-1a: only guards against garbage after a power cut.
    b.iter()
        .fold(0x811c_9dc5u32, |h, &x| (h ^ x as u32).wrapping_mul(0x0100_0193))
}

fn staged() -> &'static mut Staged {
    // SAFETY: only core 0 touches it, at boot and in the hub task.
    unsafe { &mut *core::ptr::addr_of_mut!(STAGED) }
}

/// Keep `g` for the next boot. False if it does not fit.
pub fn stage(g: &Gadget) -> bool {
    let bytes = g.to_bytes();
    if bytes.len() > CAP {
        return false;
    }
    let s = staged();
    s.magic = 0;
    s.data[..bytes.len()].copy_from_slice(&bytes);
    s.len = bytes.len() as u32;
    s.sum = checksum(&bytes);
    s.dirty = 1;
    s.magic = MAGIC;
    true
}

/// The gadget to present. Call at boot, before core 1 starts: it may write
/// the flash.
pub fn load() -> Option<Gadget> {
    let s = staged();
    let n = s.len as usize;
    if s.magic == MAGIC && n <= CAP && checksum(&s.data[..n]) == s.sum {
        let bytes = &s.data[..n];
        if let Some(g) = Gadget::from_bytes(bytes) {
            if s.dirty == 1 {
                match write_flash(bytes) {
                    Ok(()) => log::info!("gadget saved to flash ({n} bytes)"),
                    Err(rc) => log::warn!("gadget not saved to flash: {rc}"),
                }
                s.dirty = 0;
            }
            return Some(g);
        }
    }
    s.magic = 0;
    let g = read_flash();
    if let Some(g) = &g {
        // Keep it in RTC memory too: the next resets skip the flash read.
        stage(g);
        staged().dirty = 0;
    }
    g
}

fn words() -> Box<[u32; SECTOR / 4]> {
    Box::new([u32::MAX; SECTOR / 4])
}

fn as_bytes(w: &mut [u32; SECTOR / 4]) -> &mut [u8] {
    // SAFETY: `SECTOR / 4` initialized u32s are `SECTOR` bytes.
    unsafe { core::slice::from_raw_parts_mut(w.as_mut_ptr().cast::<u8>(), SECTOR) }
}

fn read_flash() -> Option<Gadget> {
    let mut w = words();
    let rc = critical_section::with(|_| unsafe {
        esp_rom_sys::rom::spiflash::esp_rom_spiflash_read(FLASH_ADDR, w.as_mut_ptr(), SECTOR as u32)
    });
    if rc != 0 {
        log::warn!("gadget: flash read failed ({rc})");
        return None;
    }
    let (magic, len, sum) = (w[0], w[1] as usize, w[2]);
    if magic != MAGIC || len > SECTOR - HEADER {
        return None;
    }
    let bytes = &as_bytes(&mut w)[HEADER..HEADER + len];
    if checksum(bytes) != sum {
        log::warn!("gadget: bad checksum in flash");
        return None;
    }
    Gadget::from_bytes(bytes)
}

fn write_flash(bytes: &[u8]) -> Result<(), i32> {
    use esp_rom_sys::rom::spiflash::{
        esp_rom_spiflash_erase_sector, esp_rom_spiflash_unlock, esp_rom_spiflash_write,
    };
    let mut w = words();
    w[0] = MAGIC;
    w[1] = bytes.len() as u32;
    w[2] = checksum(bytes);
    as_bytes(&mut w)[HEADER..HEADER + bytes.len()].copy_from_slice(bytes);
    let rc = critical_section::with(|_| unsafe { esp_rom_spiflash_unlock() });
    if rc != 0 {
        return Err(rc);
    }
    let rc = critical_section::with(|_| unsafe {
        esp_rom_spiflash_erase_sector(FLASH_ADDR / SECTOR as u32)
    });
    if rc != 0 {
        return Err(rc);
    }
    const PAGE: usize = 256;
    let used = (HEADER + bytes.len()).div_ceil(PAGE);
    for page in 0..used {
        let at = FLASH_ADDR + (page * PAGE) as u32;
        let ptr = unsafe { w.as_ptr().add(page * PAGE / 4) };
        let rc = critical_section::with(|_| unsafe { esp_rom_spiflash_write(at, ptr, PAGE as u32) });
        if rc != 0 {
            return Err(rc);
        }
    }
    Ok(())
}
