//! Raw evdev mouse reader (from dsu-mouse): one thread per device, summed into a
//! shared accumulator.

use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::raw::{c_int, c_ulong};
use std::io::Read;
use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;

const EV_KEY: u16 = 0x01;
const EV_REL: u16 = 0x02;
const REL_X: u16 = 0x00;
const REL_Y: u16 = 0x01;
const REL_WHEEL: u16 = 0x08;

const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;
const BTN_MIDDLE: u16 = 0x112;
const BTN_SIDE: u16 = 0x113;
const BTN_EXTRA: u16 = 0x114;

/// `struct input_event` on 64-bit Linux: two 64-bit timeval fields, then type,
/// code and value.
const EVENT_SIZE: usize = 24;

/// Movement accumulated since the last send, plus current button state.
///
/// Mouse motion is a displacement, so the only correct way to consume it is to drain
/// the whole accumulator each tick. Sampling it would throw away counts.
#[derive(Default)]
pub struct Accumulator {
    dx: AtomicI64,
    dy: AtomicI64,
    wheel: AtomicI64,
    buttons: AtomicU32,
}

/// One drained batch of input.
pub struct Batch {
    pub dx: i64,
    pub dy: i64,
    pub wheel: i64,
    pub buttons: u32,
}

impl Accumulator {
    /// Takes everything accumulated so far, resetting the deltas. Button state is
    /// level triggered so it is read, not cleared.
    pub fn drain(&self) -> Batch {
        Batch {
            dx: self.dx.swap(0, Ordering::Relaxed),
            dy: self.dy.swap(0, Ordering::Relaxed),
            wheel: self.wheel.swap(0, Ordering::Relaxed),
            buttons: self.buttons.load(Ordering::Relaxed),
        }
    }
}

/// A mouse-like device found in `/proc/bus/input/devices`.
pub struct Device {
    #[allow(dead_code)]
    pub name: String,
    pub path: String,
}

/// Finds relative pointing devices.
///
/// The kernel's mousedev only attaches a `mouseN` handler to devices that report
/// REL_X, REL_Y and a button, which is exactly the set we want, so the presence of
/// that handler is the whole test.
pub fn discover() -> std::io::Result<Vec<Device>> {
    let mut text = String::new();
    File::open("/proc/bus/input/devices")?.read_to_string(&mut text)?;

    let mut devices = Vec::new();
    let mut name = String::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("N: Name=") {
            name = rest.trim().trim_matches('"').to_string();
        } else if let Some(rest) = line.strip_prefix("H: Handlers=") {
            let handlers: Vec<&str> = rest.split_whitespace().collect();
            if !handlers.iter().any(|h| h.starts_with("mouse")) {
                continue;
            }
            if let Some(ev) = handlers.iter().find(|h| h.starts_with("event")) {
                devices.push(Device {
                    name: std::mem::take(&mut name),
                    path: format!("/dev/input/{ev}"),
                });
            }
        }
    }
    Ok(devices)
}

extern "C" {
    fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
}

/// `_IOW('E', 0x90, int)`.
const EVIOCGRAB: c_ulong = 0x4004_4590;

/// Handles for exclusively grabbing the opened devices.
///
/// An exclusive grab stops the kernel from delivering this device's events to anyone
/// else, the compositor included, so the desktop pointer stays put and clicks cannot
/// land outside the game. The grab belongs to the open file description, so these
/// duplicated handles grab the same thing the reader threads read from, and the
/// kernel drops it automatically if this process dies.
pub struct Grabber {
    files: Vec<File>,
}

impl Grabber {
    pub fn set(&self, grab: bool) {
        for f in &self.files {
            // SAFETY: plain ioctl on a file descriptor we own, integer argument.
            let rc = unsafe { ioctl(f.as_raw_fd(), EVIOCGRAB, grab as c_int) };
            if rc != 0 {
                eprintln!("EVIOCGRAB({grab}) failed: {}", std::io::Error::last_os_error());
            }
        }
    }
}

/// Spawns a reader thread per device. Returns the shared accumulator, a grabber for
/// the opened devices and the number of devices successfully opened.
///
/// A device that cannot be opened is reported and skipped rather than fatal, since a
/// machine usually has several and only some matter.
pub fn spawn_readers(paths: &[String]) -> (Arc<Accumulator>, Grabber, usize) {
    let acc = Arc::new(Accumulator::default());
    let mut grab_files = Vec::new();
    let mut opened = 0;

    for path in paths {
        let file = match File::open(path) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("  {path}: {e}");
                continue;
            }
        };
        opened += 1;
        if let Ok(dup) = file.try_clone() {
            grab_files.push(dup);
        }
        let acc = Arc::clone(&acc);
        let path = path.clone();
        thread::spawn(move || read_loop(file, &acc, &path));
    }

    (acc, Grabber { files: grab_files }, opened)
}

fn read_loop(mut file: File, acc: &Accumulator, path: &str) {
    let mut buf = [0u8; EVENT_SIZE * 32];
    loop {
        let n = match file.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => n,
            Err(e) => {
                eprintln!("{path}: read failed: {e}");
                return;
            }
        };
        for chunk in buf[..n].chunks_exact(EVENT_SIZE) {
            let ev_type = u16::from_le_bytes(chunk[16..18].try_into().unwrap());
            let code = u16::from_le_bytes(chunk[18..20].try_into().unwrap());
            let value = i32::from_le_bytes(chunk[20..24].try_into().unwrap());

            match (ev_type, code) {
                (EV_REL, REL_X) => {
                    acc.dx.fetch_add(value as i64, Ordering::Relaxed);
                }
                (EV_REL, REL_Y) => {
                    acc.dy.fetch_add(value as i64, Ordering::Relaxed);
                }
                (EV_REL, REL_WHEEL) => {
                    acc.wheel.fetch_add(value as i64, Ordering::Relaxed);
                }
                (EV_KEY, c) => {
                    if let Some(bit) = button_bit(c) {
                        if value != 0 {
                            acc.buttons.fetch_or(bit, Ordering::Relaxed);
                        } else {
                            acc.buttons.fetch_and(!bit, Ordering::Relaxed);
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

fn button_bit(code: u16) -> Option<u32> {
    match code {
        BTN_LEFT => Some(1 << 0),
        BTN_RIGHT => Some(1 << 1),
        BTN_MIDDLE => Some(1 << 2),
        BTN_SIDE => Some(1 << 3),
        BTN_EXTRA => Some(1 << 4),
        _ => None,
    }
}
