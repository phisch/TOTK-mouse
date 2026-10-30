//! evdev fallback: reads /dev/input directly and grabs with EVIOCGRAB.

use super::evdev_reader as reader;
use std::sync::Arc;

pub struct Evdev {
    acc: Arc<reader::Accumulator>,
    grabber: reader::Grabber,
    grabbed: bool,
}

impl Evdev {
    pub fn new() -> Result<Self, String> {
        let paths: Vec<String> = reader::discover()
            .map_err(|e| format!("cannot read /proc/bus/input/devices: {e}"))?
            .into_iter()
            .map(|d| d.path)
            .collect();
        let (acc, grabber, opened) = reader::spawn_readers(&paths);
        if opened == 0 {
            return Err("no mouse under /dev/input could be opened (needs the 'input' group or a udev rule)".into());
        }
        Ok(Self { acc, grabber, grabbed: false })
    }
}

impl super::Backend for Evdev {
    fn name(&self) -> &'static str {
        "evdev"
    }

    fn poll(&mut self) -> super::Batch {
        let b = self.acc.drain();
        super::Batch { dx: b.dx as f64, dy: b.dy as f64, wheel: b.wheel as i32, buttons: b.buttons, ..Default::default() }
    }

    fn attach(&mut self, _pids: &[u32]) -> bool {
        true
    }

    fn attached(&self) -> bool {
        true
    }

    fn set_grab(&mut self, grab: bool) -> bool {
        if grab != self.grabbed {
            self.grabber.set(grab);
            self.grabbed = grab;
        }
        self.grabbed
    }
}
