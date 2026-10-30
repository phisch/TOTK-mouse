//! Mouse input backends.
//!
//! X11 is the default: yuzu forks run under XWayland, any X client can read raw
//! (unaccelerated) motion while the emulator window has focus, and an X pointer grab
//! confines and hides the cursor. None of that needs special permissions. evdev is
//! the fallback for when the emulator has no X11 window; it reads /dev/input and so
//! needs the `input` group or a udev rule.
//!
//! On Windows, Raw Input gives the same unaccelerated motion and ClipCursor confines
//! the pointer.

#[cfg(target_os = "linux")]
pub mod evdev;
#[cfg(target_os = "linux")]
mod evdev_reader;
#[cfg(windows)]
pub mod rawinput;
#[cfg(target_os = "linux")]
pub mod x11;

/// Input since the last poll.
#[derive(Default)]
pub struct Batch {
    /// Raw motion, mouse counts.
    pub dx: f64,
    pub dy: f64,
    /// Wheel notches, positive is up.
    pub wheel: i32,
    /// Buttons held right now, bits as in `config::mouse_button_bit`.
    pub buttons: u32,
    /// Bound keys pressed since the last poll, and held right now.
    pub keys_pressed: Vec<String>,
    pub keys_held: Vec<String>,
}

/// Whether the platform's key backend knows this key name.
pub fn key_supported(name: &str) -> bool {
    #[cfg(target_os = "linux")]
    return x11::keysym(name).is_some();
    #[cfg(windows)]
    return rawinput::vk_code(name).is_some();
}

pub trait Backend: Send {
    fn name(&self) -> &'static str;
    fn poll(&mut self) -> Batch;
    /// Finds the window to confine the pointer to, owned by one of `pids`. Backends
    /// without windows always succeed.
    fn attach(&mut self, pids: &[u32]) -> bool;
    fn attached(&self) -> bool;
    /// Grabs (confines and hides) or releases the pointer. Calling it again while
    /// grabbed re-asserts the grab. Returns whether the pointer is grabbed now.
    fn set_grab(&mut self, grab: bool) -> bool;
    /// Sets the keys to report. Backends without keyboard input ignore it.
    fn set_keys(&mut self, _keys: &[String]) {}
}
