//! The mod's shared block in guest memory.
//!
//! Emulators have no clean channel for relative mouse input into guest code (see
//! docs/how-it-works.md), so we write straight into guest memory: the mod keeps a small signed
//! block in its data, and we find it by signature in the emulator's guest RAM.

use crate::process::{descendants, Process};

/// Must match `SharedBlock` in mod/source/program/main.cpp.
const MAGIC: &[u8; 8] = b"TMOUSE01";
const VERSION: u32 = 6;
const COOKIE: u32 = 0x5a17_c3e9;
const OFF_TOTAL_X: u64 = 16;
const OFF_SEQ: u64 = 32;
const OFF_ENABLED: u64 = 36;
const OFF_SENS_X: u64 = 40;
const OFF_HEARTBEAT: u64 = 44;
const OFF_PAD_BUTTONS: u64 = 52;
const OFF_SENS_Y: u64 = 60;

pub struct Block {
    pub pid: u32,
    mem: Process,
    at: u64,
    seq: u32,
}

impl Block {
    /// Finds the block in `root` or any of its descendants (the launched process may
    /// be a wrapper, an AppImage launcher for example).
    pub fn locate(root: u32) -> Option<Block> {
        for pid in descendants(root) {
            let Some(mem) = Process::open(pid) else { continue };
            let regions = mem.guest_ram_regions();
            if let Some(at) = find_block(&mem, &regions) {
                return Some(Block { pid, mem, at, seq: 0 });
            }
        }
        None
    }

    /// The block moves if the game reboots inside the emulator. Writing to a stale
    /// address would scribble over arbitrary guest memory, so check before writing.
    pub fn valid(&self) -> bool {
        let mut b = [0u8; 16];
        self.mem.read_at(&mut b, self.at) == 16 && b == signature()
    }

    pub fn heartbeat(&self) -> u32 {
        let mut b = [0u8; 4];
        self.mem.read_at(&mut b, self.at + OFF_HEARTBEAT);
        u32::from_le_bytes(b)
    }

    pub fn set_enabled(&self, on: bool) {
        self.write(OFF_ENABLED, &(on as u32).to_le_bytes());
    }

    pub fn set_sensitivity(&self, x: f32, y: f32) {
        self.write(OFF_SENS_X, &x.to_le_bytes());
        self.write(OFF_SENS_Y, &y.to_le_bytes());
    }

    pub fn set_pad_buttons(&self, bits: u32) {
        self.write(OFF_PAD_BUTTONS, &bits.to_le_bytes());
    }

    /// Totals and sequence number in one write: they are adjacent in the block.
    pub fn set_totals(&mut self, x: i64, y: i64) {
        self.seq = self.seq.wrapping_add(1);
        let mut b = [0u8; 20];
        b[..8].copy_from_slice(&x.to_le_bytes());
        b[8..16].copy_from_slice(&y.to_le_bytes());
        b[16..].copy_from_slice(&self.seq.to_le_bytes());
        debug_assert_eq!(OFF_SEQ, OFF_TOTAL_X + 16);
        self.write(OFF_TOTAL_X, &b);
    }

    fn write(&self, off: u64, bytes: &[u8]) {
        if let Err(e) = self.mem.write_at(bytes, self.at + off) {
            eprintln!("write to guest failed: {e}");
        }
    }
}

fn signature() -> [u8; 16] {
    let mut s = [0u8; 16];
    s[..8].copy_from_slice(MAGIC);
    s[8..12].copy_from_slice(&VERSION.to_le_bytes());
    s[12..].copy_from_slice(&COOKIE.to_le_bytes());
    s
}

fn find_block(mem: &Process, regions: &[(u64, u64)]) -> Option<u64> {
    const CHUNK: u64 = 32 << 20;
    let sig = signature();
    let mut buf = vec![0u8; CHUNK as usize];
    for &(start, end) in regions {
        let mut addr = start;
        while addr < end {
            let n = CHUNK.min(end - addr) as usize;
            let got = mem.read_at(&mut buf[..n], addr);
            // The block is 64-byte aligned, so only check aligned offsets.
            let mut i = 0;
            while i + 16 <= got {
                if buf[i..i + 16] == sig {
                    return Some(addr + i as u64);
                }
                i += 64;
            }
            addr += n as u64;
        }
    }
    None
}
