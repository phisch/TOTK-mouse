//! X11 input: XI2 raw events and an X pointer grab.
//!
//! Raw events (XI_RawMotion and friends, selected on the root window) carry
//! unaccelerated motion at the mouse's full polling rate. Under XWayland they only
//! arrive while an XWayland window has focus, which is exactly when the emulator is
//! in front. Raw events keep coming while the pointer is pinned at an edge, and they
//! are delivered during our own grab too.
//!
//! A grab with `confine_to` set is what XWayland turns into Wayland pointer
//! confinement. Pointer barriers would let the emulator keep receiving clicks, but
//! XWayland does not enforce them, so the grab it is. The emulator then sees no mouse
//! at all, which is why buttons are forwarded through the mod instead.

use x11rb::connection::Connection;
use x11rb::protocol::res::{self, ConnectionExt as _};
use x11rb::protocol::xinput::{self, ConnectionExt as _};
use x11rb::protocol::xproto::*;
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;

pub struct X11 {
    conn: RustConnection,
    root: Window,
    pid_atom: Atom,
    /// The X-Resource extension knows which process owns a window even when the
    /// client sets no _NET_WM_PID (Ryujinx's Avalonia window does not).
    has_xres: bool,
    cursor: Cursor,
    /// Bound keys: (keycode, name).
    keys: Vec<(u8, String)>,
    held_keys: Vec<u8>,
    target: Option<Window>,
    grabbed: bool,
    buttons: u32,
    /// Fractional raw motion not yet handed out, so nothing is lost to rounding.
    rem_x: f64,
    rem_y: f64,
}

fn fp3232(v: xinput::Fp3232) -> f64 {
    v.integral as f64 + v.frac as f64 / 4294967296.0
}

fn button_bit(detail: u32) -> u32 {
    match detail {
        1 => 1 << 0,
        3 => 1 << 1,
        2 => 1 << 2,
        8 => 1 << 3,
        9 => 1 << 4,
        _ => 0,
    }
}

/// Keysym for a key name: F1..F12, a few navigation keys, letters and digits.
pub fn keysym(name: &str) -> Option<u32> {
    if let Some(n) = name.strip_prefix(['F', 'f']).and_then(|n| n.parse::<u32>().ok()) {
        return (1..=12).contains(&n).then(|| 0xffbe + n - 1);
    }
    let sym = match name.to_ascii_lowercase().as_str() {
        "pause" => 0xff13,
        "scroll_lock" | "scrolllock" => 0xff14,
        "insert" => 0xff63,
        "home" => 0xff50,
        "end" => 0xff57,
        "page_up" | "pageup" => 0xff55,
        "page_down" | "pagedown" => 0xff56,
        s if s.len() == 1 && s.as_bytes()[0].is_ascii_alphanumeric() => s.as_bytes()[0] as u32,
        _ => return None,
    };
    Some(sym)
}

impl X11 {
    pub fn new(keys: &[String]) -> Result<Self, String> {
        let (conn, screen) = x11rb::connect(None).map_err(|e| format!("cannot connect to X11: {e}"))?;
        let root = conn.setup().roots[screen].root;
        let v = conn
            .xinput_xi_query_version(2, 2)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| format!("XInput2 unavailable: {e}"))?;
        if (v.major_version, v.minor_version) < (2, 2) {
            return Err(format!("XInput {}.{} is too old, need 2.2", v.major_version, v.minor_version));
        }
        let mask = xinput::XIEventMask::RAW_MOTION
            | xinput::XIEventMask::RAW_BUTTON_PRESS
            | xinput::XIEventMask::RAW_BUTTON_RELEASE
            | xinput::XIEventMask::RAW_KEY_PRESS
            | xinput::XIEventMask::RAW_KEY_RELEASE;
        conn.xinput_xi_select_events(root, &[xinput::EventMask { deviceid: 1, mask: vec![mask] }])
            .map_err(|e| e.to_string())?;
        let pid_atom = conn.intern_atom(false, b"_NET_WM_PID").map_err(|e| e.to_string())?
            .reply().map_err(|e| e.to_string())?.atom;

        // An invisible cursor for while we hold the grab.
        let pix = conn.generate_id().map_err(|e| e.to_string())?;
        conn.create_pixmap(1, pix, root, 1, 1).map_err(|e| e.to_string())?;
        let cursor = conn.generate_id().map_err(|e| e.to_string())?;
        conn.create_cursor(cursor, pix, pix, 0, 0, 0, 0, 0, 0, 0, 0).map_err(|e| e.to_string())?;
        conn.free_pixmap(pix).map_err(|e| e.to_string())?;

        let has_xres = conn.res_query_version(1, 2).ok().and_then(|c| c.reply().ok()).is_some();
        let mut x = Self {
            conn,
            root,
            pid_atom,
            has_xres,
            cursor,
            keys: Vec::new(),
            held_keys: Vec::new(),
            target: None,
            grabbed: false,
            buttons: 0,
            rem_x: 0.0,
            rem_y: 0.0,
        };
        super::Backend::set_keys(&mut x, keys);
        x.conn.flush().map_err(|e| e.to_string())?;
        Ok(x)
    }

    fn keycode(&self, name: &str) -> Option<u8> {
        let sym = keysym(name)?;
        let setup = self.conn.setup();
        let (min, max) = (setup.min_keycode, setup.max_keycode);
        let map = self.conn.get_keyboard_mapping(min, max - min + 1).ok()?.reply().ok()?;
        let per = map.keysyms_per_keycode as usize;
        map.keysyms.chunks(per).position(|syms| syms.contains(&sym)).map(|i| min + i as u8)
    }

    fn window_pid(&self, w: Window) -> Option<u32> {
        let r = self.conn.get_property(false, w, self.pid_atom, AtomEnum::CARDINAL, 0, 1).ok()?.reply().ok()?;
        if let Some(pid) = r.value32().and_then(|mut v| v.next()) {
            return Some(pid);
        }
        if !self.has_xres {
            return None;
        }
        let spec = res::ClientIdSpec { client: w, mask: res::ClientIdMask::LOCAL_CLIENT_PID };
        let ids = self.conn.res_query_client_ids(&[spec]).ok()?.reply().ok()?;
        ids.ids.first().and_then(|id| id.value.first().copied())
    }

    fn collect(&self, w: Window, pids: &[u32], out: &mut Vec<Window>) {
        if self.window_pid(w).is_some_and(|p| pids.contains(&p)) {
            out.push(w);
        }
        if let Ok(Ok(t)) = self.conn.query_tree(w).map(|c| c.reply()) {
            for c in t.children {
                self.collect(c, pids, out);
            }
        }
    }
}

impl super::Backend for X11 {
    fn name(&self) -> &'static str {
        "X11"
    }

    fn poll(&mut self) -> super::Batch {
        let mut b = super::Batch::default();
        let (mut dx, mut dy) = (self.rem_x, self.rem_y);
        while let Ok(Some(ev)) = self.conn.poll_for_event() {
            match ev {
                Event::XinputRawMotion(e) => {
                    let mask = e.valuator_mask.first().copied().unwrap_or(0);
                    let mut i = 0;
                    for axis in 0..2 {
                        if mask & (1 << axis) != 0 {
                            let v = e.axisvalues_raw.get(i).copied().map(fp3232).unwrap_or(0.0);
                            if axis == 0 { dx += v } else { dy += v }
                            i += 1;
                        }
                    }
                }
                Event::XinputRawButtonPress(e) => match e.detail {
                    4 => b.wheel += 1,
                    5 => b.wheel -= 1,
                    d => self.buttons |= button_bit(d),
                },
                Event::XinputRawButtonRelease(e) => self.buttons &= !button_bit(e.detail),
                Event::XinputRawKeyPress(e) => {
                    let code = e.detail as u8;
                    if let Some((_, name)) = self.keys.iter().find(|(c, _)| *c == code) {
                        if !self.held_keys.contains(&code) {
                            self.held_keys.push(code);
                            b.keys_pressed.push(name.clone());
                        }
                    }
                }
                Event::XinputRawKeyRelease(e) => self.held_keys.retain(|c| *c != e.detail as u8),
                _ => {}
            }
        }
        b.dx = dx.trunc();
        b.dy = dy.trunc();
        self.rem_x = dx - b.dx;
        self.rem_y = dy - b.dy;
        b.buttons = self.buttons;
        b.keys_held = self
            .keys
            .iter()
            .filter(|(c, _)| self.held_keys.contains(c))
            .map(|(_, n)| n.clone())
            .collect();
        b
    }

    fn attach(&mut self, pids: &[u32]) -> bool {
        let mut wins = Vec::new();
        self.collect(self.root, pids, &mut wins);
        // The largest viewable window is the emulator's main window.
        let mut best: Option<(Window, u32)> = None;
        for w in wins {
            let Ok(Ok(a)) = self.conn.get_window_attributes(w).map(|c| c.reply()) else { continue };
            if a.map_state != MapState::VIEWABLE {
                continue;
            }
            let Ok(Ok(g)) = self.conn.get_geometry(w).map(|c| c.reply()) else { continue };
            let area = g.width as u32 * g.height as u32;
            if best.map_or(true, |(_, a)| area > a) {
                best = Some((w, area));
            }
        }
        self.target = best.map(|(w, _)| w);
        self.target.is_some()
    }

    fn attached(&self) -> bool {
        self.target.is_some()
    }

    fn set_keys(&mut self, keys: &[String]) {
        self.held_keys.clear();
        self.keys.clear();
        for k in keys {
            match self.keycode(k) {
                Some(code) => self.keys.push((code, k.clone())),
                None => eprintln!("config: unknown key {k:?}"),
            }
        }
    }

    fn set_grab(&mut self, grab: bool) -> bool {
        if !grab {
            if self.grabbed {
                let _ = self.conn.ungrab_pointer(x11rb::CURRENT_TIME);
                let _ = self.conn.flush();
                self.grabbed = false;
            }
            return false;
        }
        let Some(w) = self.target else { return false };
        let status = self
            .conn
            .grab_pointer(false, w, EventMask::NO_EVENT, GrabMode::ASYNC, GrabMode::ASYNC, w, self.cursor, x11rb::CURRENT_TIME)
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|r| r.status);
        self.grabbed = status == Some(GrabStatus::SUCCESS);
        if status == Some(GrabStatus::NOT_VIEWABLE) || status.is_none() {
            // The window went away or was replaced: find it again next time.
            self.target = None;
        }
        self.grabbed
    }
}
