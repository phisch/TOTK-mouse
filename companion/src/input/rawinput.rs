//! Windows input: Raw Input and ClipCursor.
//!
//! Raw Input (WM_INPUT, registered with RIDEV_INPUTSINK on a hidden message window)
//! delivers unaccelerated mouse motion, buttons, wheel and keys whichever window has
//! focus. We only count it while the emulator is in front, like the X11 backend.
//! The lock confines the cursor to one pixel in the middle of the emulator window.
//! Unlike the X11 grab this does not hide clicks from the emulator.

use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use std::sync::{Arc, Mutex, OnceLock};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::{
    GetRawInputData, RegisterRawInputDevices, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER, RIDEV_INPUTSINK,
    RID_INPUT, RIM_TYPEKEYBOARD, RIM_TYPEMOUSE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    ClipCursor, CreateWindowExW, DefWindowProcW, DispatchMessageW, EnumWindows, GetClientRect, GetForegroundWindow,
    GetMessageW, GetWindowThreadProcessId, IsWindowVisible, RegisterClassW, HWND_MESSAGE, MSG, WM_INPUT, WNDCLASSW,
};

const RI_MOUSE_LEFT_DOWN: u16 = 0x0001;
const RI_MOUSE_LEFT_UP: u16 = 0x0002;
const RI_MOUSE_RIGHT_DOWN: u16 = 0x0004;
const RI_MOUSE_RIGHT_UP: u16 = 0x0008;
const RI_MOUSE_MIDDLE_DOWN: u16 = 0x0010;
const RI_MOUSE_MIDDLE_UP: u16 = 0x0020;
const RI_MOUSE_BUTTON_4_DOWN: u16 = 0x0040;
const RI_MOUSE_BUTTON_4_UP: u16 = 0x0080;
const RI_MOUSE_BUTTON_5_DOWN: u16 = 0x0100;
const RI_MOUSE_BUTTON_5_UP: u16 = 0x0200;
const RI_MOUSE_WHEEL: u16 = 0x0400;
const MOUSE_MOVE_ABSOLUTE: u16 = 0x0001;
const RI_KEY_BREAK: u16 = 0x0001;
const WHEEL_DELTA: i32 = 120;

/// Virtual-key code for a key name: F1..F24, a few navigation keys, letters, digits.
pub fn vk_code(name: &str) -> Option<u16> {
    if let Some(n) = name.strip_prefix(['F', 'f']).and_then(|n| n.parse::<u16>().ok()) {
        return (1..=24).contains(&n).then(|| 0x70 + n - 1);
    }
    let vk = match name.to_ascii_lowercase().as_str() {
        "pause" => 0x13,
        "scroll_lock" | "scrolllock" => 0x91,
        "insert" => 0x2d,
        "home" => 0x24,
        "end" => 0x23,
        "page_up" | "pageup" => 0x21,
        "page_down" | "pagedown" => 0x22,
        s if s.len() == 1 && s.as_bytes()[0].is_ascii_alphanumeric() => s.as_bytes()[0].to_ascii_uppercase() as u16,
        _ => return None,
    };
    Some(vk)
}

#[derive(Default)]
struct Inner {
    dx: f64,
    dy: f64,
    wheel_acc: i32,
    wheel: i32,
    buttons: u32,
    /// Bound keys: (virtual-key code, name).
    keys: Vec<(u16, String)>,
    held_keys: Vec<u16>,
    pressed: Vec<String>,
    /// Emulator processes: input only counts while one of them is in front.
    pids: Vec<u32>,
}

static STATE: OnceLock<Arc<Mutex<Inner>>> = OnceLock::new();

fn foreground_pid() -> u32 {
    let mut pid = 0;
    // SAFETY: plain calls; a null window yields pid 0.
    unsafe { GetWindowThreadProcessId(GetForegroundWindow(), &mut pid) };
    pid
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_INPUT {
        if let Some(state) = STATE.get() {
            handle_input(lparam as HRAWINPUT, state);
        }
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

fn handle_input(h: HRAWINPUT, state: &Mutex<Inner>) {
    // SAFETY: RAWINPUT is plain data and large enough for mouse and keyboard input.
    let mut raw: RAWINPUT = unsafe { zeroed() };
    let mut size = size_of::<RAWINPUT>() as u32;
    // SAFETY: valid handle from WM_INPUT and a buffer of `size` bytes.
    let n = unsafe {
        GetRawInputData(h, RID_INPUT, (&mut raw as *mut RAWINPUT).cast(), &mut size, size_of::<RAWINPUTHEADER>() as u32)
    };
    if n == u32::MAX || n == 0 {
        return;
    }
    let mut s = state.lock().unwrap();
    if !s.pids.contains(&foreground_pid()) {
        return;
    }
    if raw.header.dwType == RIM_TYPEMOUSE {
        // SAFETY: dwType says this is the mouse variant.
        let m = unsafe { raw.data.mouse };
        if m.usFlags & MOUSE_MOVE_ABSOLUTE == 0 {
            s.dx += m.lLastX as f64;
            s.dy += m.lLastY as f64;
        }
        // SAFETY: reading the button flags view of the union.
        let (flags, data) = unsafe { (m.Anonymous.Anonymous.usButtonFlags, m.Anonymous.Anonymous.usButtonData) };
        for (down, up, bit) in [
            (RI_MOUSE_LEFT_DOWN, RI_MOUSE_LEFT_UP, 1 << 0),
            (RI_MOUSE_RIGHT_DOWN, RI_MOUSE_RIGHT_UP, 1 << 1),
            (RI_MOUSE_MIDDLE_DOWN, RI_MOUSE_MIDDLE_UP, 1 << 2),
            (RI_MOUSE_BUTTON_4_DOWN, RI_MOUSE_BUTTON_4_UP, 1 << 3),
            (RI_MOUSE_BUTTON_5_DOWN, RI_MOUSE_BUTTON_5_UP, 1 << 4),
        ] {
            if flags & down != 0 {
                s.buttons |= bit;
            }
            if flags & up != 0 {
                s.buttons &= !bit;
            }
        }
        if flags & RI_MOUSE_WHEEL != 0 {
            // High resolution wheels send fractions of a notch.
            s.wheel_acc += data as i16 as i32;
            let notches = s.wheel_acc / WHEEL_DELTA;
            s.wheel += notches;
            s.wheel_acc -= notches * WHEEL_DELTA;
        }
    } else if raw.header.dwType == RIM_TYPEKEYBOARD {
        // SAFETY: dwType says this is the keyboard variant.
        let k = unsafe { raw.data.keyboard };
        let vk = k.VKey;
        let Some(name) = s.keys.iter().find(|(c, _)| *c == vk).map(|(_, n)| n.clone()) else { return };
        if k.Flags & RI_KEY_BREAK != 0 {
            s.held_keys.retain(|c| *c != vk);
        } else if !s.held_keys.contains(&vk) {
            s.held_keys.push(vk);
            s.pressed.push(name);
        }
    }
}

/// Runs the hidden window and its message loop. Never returns.
fn message_thread(ready: std::sync::mpsc::Sender<Result<(), String>>) {
    let class: Vec<u16> = "totk-mouse-rawinput\0".encode_utf16().collect();
    // SAFETY: standard window class registration and creation with valid pointers.
    let hwnd = unsafe {
        let hinst = GetModuleHandleW(null());
        let mut wc: WNDCLASSW = zeroed();
        wc.lpfnWndProc = Some(wndproc);
        wc.hInstance = hinst;
        wc.lpszClassName = class.as_ptr();
        RegisterClassW(&wc);
        CreateWindowExW(0, class.as_ptr(), class.as_ptr(), 0, 0, 0, 0, 0, HWND_MESSAGE, null_mut(), hinst, null())
    };
    if hwnd.is_null() {
        let _ = ready.send(Err("cannot create the Raw Input window".into()));
        return;
    }
    let devices = [
        RAWINPUTDEVICE { usUsagePage: 1, usUsage: 2, dwFlags: RIDEV_INPUTSINK, hwndTarget: hwnd },
        RAWINPUTDEVICE { usUsagePage: 1, usUsage: 6, dwFlags: RIDEV_INPUTSINK, hwndTarget: hwnd },
    ];
    // SAFETY: valid array of two devices.
    let ok = unsafe { RegisterRawInputDevices(devices.as_ptr(), 2, size_of::<RAWINPUTDEVICE>() as u32) };
    if ok == 0 {
        let _ = ready.send(Err(format!("RegisterRawInputDevices failed: {}", std::io::Error::last_os_error())));
        return;
    }
    let _ = ready.send(Ok(()));
    // SAFETY: standard message loop on the thread that owns the window.
    unsafe {
        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            DispatchMessageW(&msg);
        }
    }
}

pub struct RawInput {
    state: Arc<Mutex<Inner>>,
    target: Option<isize>,
    grabbed: bool,
}

impl RawInput {
    pub fn new(keys: &[String]) -> Result<Self, String> {
        let state = Arc::new(Mutex::new(Inner::default()));
        STATE.set(state.clone()).map_err(|_| "Raw Input already started".to_string())?;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || message_thread(tx));
        rx.recv().map_err(|_| "Raw Input thread died".to_string())??;
        let mut r = Self { state, target: None, grabbed: false };
        super::Backend::set_keys(&mut r, keys);
        Ok(r)
    }
}

struct EnumCtx<'a> {
    pids: &'a [u32],
    best: Option<(HWND, i64)>,
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> i32 {
    let ctx = &mut *(lparam as *mut EnumCtx);
    let mut pid = 0;
    GetWindowThreadProcessId(hwnd, &mut pid);
    if IsWindowVisible(hwnd) != 0 && ctx.pids.contains(&pid) {
        let mut r: RECT = zeroed();
        GetClientRect(hwnd, &mut r);
        let area = (r.right - r.left) as i64 * (r.bottom - r.top) as i64;
        if ctx.best.map_or(true, |(_, a)| area > a) {
            ctx.best = Some((hwnd, area));
        }
    }
    1
}

impl super::Backend for RawInput {
    fn name(&self) -> &'static str {
        "Raw Input"
    }

    fn poll(&mut self) -> super::Batch {
        let mut s = self.state.lock().unwrap();
        let mut b = super::Batch::default();
        b.dx = s.dx.trunc();
        b.dy = s.dy.trunc();
        s.dx -= b.dx;
        s.dy -= b.dy;
        b.wheel = std::mem::take(&mut s.wheel);
        b.buttons = s.buttons;
        b.keys_pressed = std::mem::take(&mut s.pressed);
        b.keys_held = s.keys.iter().filter(|(c, _)| s.held_keys.contains(c)).map(|(_, n)| n.clone()).collect();
        b
    }

    fn attach(&mut self, pids: &[u32]) -> bool {
        self.state.lock().unwrap().pids = pids.to_vec();
        let mut ctx = EnumCtx { pids, best: None };
        // SAFETY: the callback only runs during this call, while `ctx` lives.
        unsafe { EnumWindows(Some(enum_proc), &mut ctx as *mut EnumCtx as LPARAM) };
        self.target = ctx.best.map(|(h, _)| h as isize);
        self.target.is_some()
    }

    fn attached(&self) -> bool {
        self.target.is_some()
    }

    fn set_grab(&mut self, grab: bool) -> bool {
        // Confine to whichever window is in front, as long as it belongs to the
        // emulator. Its main window is not necessarily the one we found in attach:
        // Ryujinx has several top-level windows. A clip left in place behind
        // another app would trap the user's cursor there, so never clip then.
        // SAFETY: plain call.
        let hwnd = unsafe { GetForegroundWindow() };
        let in_front = !hwnd.is_null() && self.state.lock().unwrap().pids.contains(&foreground_pid());
        if !grab || !in_front {
            if self.grabbed {
                // SAFETY: a null rect removes the clip.
                unsafe { ClipCursor(null()) };
                self.grabbed = false;
            }
            return false;
        }
        // SAFETY: valid window handle and output structs.
        unsafe {
            let mut r: RECT = zeroed();
            if GetClientRect(hwnd, &mut r) == 0 {
                return false;
            }
            let mut c = POINT { x: (r.right - r.left) / 2, y: (r.bottom - r.top) / 2 };
            ClientToScreen(hwnd, &mut c);
            let clip = RECT { left: c.x, top: c.y, right: c.x + 1, bottom: c.y + 1 };
            self.grabbed = ClipCursor(&clip) != 0;
        }
        self.grabbed
    }

    fn set_keys(&mut self, keys: &[String]) {
        let mut s = self.state.lock().unwrap();
        s.held_keys.clear();
        s.keys.clear();
        for k in keys {
            match vk_code(k) {
                Some(code) => s.keys.push((code, k.clone())),
                None => eprintln!("config: unknown key {k:?}"),
            }
        }
    }
}
