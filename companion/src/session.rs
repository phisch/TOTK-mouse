//! The companion's main loop: runs while the emulator runs, moves mouse input into
//! the mod and decides when the pointer is grabbed.
//!
//! It owns the config. The UI only reads `State` and sends `Command`s.

use crate::config::{Action, Bindings, Config, WheelTarget};
use std::collections::VecDeque;
use crate::guest::Block;
use crate::input::Backend;
use std::process::Child;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Sensitivity factor per mouse wheel notch.
const SENS_STEP: f64 = 1.02;
/// The camera counts as active while the mod's heartbeat moved this recently. It
/// stops in pause and menus, which is when the mouse should be released.
const ACTIVE_WINDOW: Duration = Duration::from_millis(400);
/// Holding a sensitivity binding repeats it after this delay, at this interval.
const REPEAT_DELAY: Duration = Duration::from_millis(400);
const REPEAT_EVERY: Duration = Duration::from_millis(50);
/// A wheel notch bound to a Switch button presses it this long, then releases it
/// for a moment so queued notches register as separate presses.
const PULSE_PRESS: Duration = Duration::from_millis(80);
const PULSE_GAP: Duration = Duration::from_millis(40);
const PULSE_QUEUE_MAX: usize = 8;
/// After a mapped button is let go, keep the grab this long while the camera comes
/// back from the menu the button opened, instead of flickering the mouse free.
const HOLD_GRACE: Duration = Duration::from_secs(1);
/// Motion goes to the mod at most this often. The game reads it once per frame and
/// the values are running totals, so nothing is lost. Each write is a syscall into
/// the emulator (under Wine even a ptrace stop), so writing on every mouse event at
/// 1000 Hz is wasteful.
const WRITE_EVERY: Duration = Duration::from_millis(4);
/// How often to re-assert the grab while it should be held. XWayland drops the
/// confinement when the window loses focus, Windows drops a cursor clip on some
/// window changes, and re-grabbing restores it.
const REGRAB_EVERY: Duration = Duration::from_millis(250);

#[derive(PartialEq, Clone, Copy, Default)]
pub struct Status {
    pub connected: bool,
    pub enabled: bool,
    pub camera_active: bool,
    pub grabbed: bool,
}

impl Status {
    pub fn line(&self) -> String {
        if !self.connected {
            return "waiting for the game (start TOTK in the emulator)".into();
        }
        let look = if self.enabled { "ON" } else { "OFF" };
        let cam = if self.camera_active { "in game" } else { "paused/menu" };
        let mouse = if self.grabbed { "grabbed" } else { "free" };
        format!("mouselook {look}, {cam}, mouse {mouse}")
    }
}

/// What the UI shows.
pub struct State {
    pub config: Config,
    pub status: Status,
    pub emulator: String,
    pub input: &'static str,
    /// Hints and problems worth showing, newest last.
    pub notices: Vec<String>,
    pub finished: bool,
}

pub type Shared = Arc<Mutex<State>>;

pub enum Command {
    /// Replace the config with this one, apply what changed and save it.
    SetConfig(Config),
    ToggleMouselook,
    Quit,
}

fn notice(shared: &Shared, msg: String) {
    println!("{msg}");
    let mut s = shared.lock().unwrap();
    s.notices.push(msg);
    let n = s.notices.len();
    if n > 5 {
        s.notices.drain(..n - 5);
    }
}

pub fn run(mut child: Child, mut backend: Box<dyn Backend>, mut cfg: Config, shared: Shared, commands: Receiver<Command>) {
    let mut bindings = Bindings::resolve(&cfg);
    let mut repeat_at: Option<Instant> = None;
    let mut pulses: VecDeque<u32> = VecDeque::new();
    let mut pulse: Option<(u32, Instant, Instant)> = None;
    let mut grace_until: Option<Instant> = None;

    let started = Instant::now();
    let mut block: Option<Block> = None;
    let mut last_locate: Option<Instant> = None;
    let mut last_validate = Instant::now();
    let mut enabled = true;
    let (mut heartbeat, mut heartbeat_at, mut last_hb_read) = (0u32, None::<Instant>, Instant::now());
    let (mut grabbed, mut last_grab) = (false, Instant::now());
    let mut last_attach: Option<Instant> = None;
    let (mut total_x, mut total_y) = (0i64, 0i64);
    let mut written = (0i64, 0i64);
    let mut last_write = Instant::now();
    let mut pad = 0u32;
    let mut prev_buttons = 0u32;
    let mut save_at: Option<Instant> = None;
    let mut status: Option<Status> = None;
    let (mut hinted_version, mut hinted_window) = (false, false);
    let mut ever_connected = false;
    let mut camera_since: Option<Instant> = None;

    let finish = |backend: &mut Box<dyn Backend>, cfg: &Config, save: bool| {
        backend.set_grab(false);
        if save {
            if let Err(e) = cfg.save() {
                eprintln!("config: {e}");
            }
        }
        shared.lock().unwrap().finished = true;
    };

    loop {
        thread::sleep(Duration::from_millis(1));
        let now = Instant::now();

        if child.try_wait().ok().flatten().is_some() {
            notice(&shared, "emulator exited".into());
            finish(&mut backend, &cfg, save_at.is_some());
            return;
        }

        let mut toggle_requested = false;
        while let Ok(cmd) = commands.try_recv() {
            match cmd {
                Command::Quit => {
                    finish(&mut backend, &cfg, save_at.is_some());
                    return;
                }
                Command::ToggleMouselook => toggle_requested = !toggle_requested,
                Command::SetConfig(new) => {
                    if new.keys != cfg.keys {
                        backend.set_keys(&new.keys.keys().cloned().collect::<Vec<_>>());
                    }
                    cfg = new;
                    bindings = Bindings::resolve(&cfg);
                    if let Some(b) = &block {
                        let (sx, sy) = cfg.signed_sensitivity();
                        b.set_sensitivity(sx, sy);
                    }
                    save_at = Some(now + Duration::from_millis(500));
                }
            }
        }

        let batch = backend.poll();

        // Find the shared block once the game has booted, and drop it if the game
        // reboots (the block moves).
        if block.is_none() && last_locate.map_or(true, |t| now - t >= Duration::from_secs(3)) {
            last_locate = Some(now);
            if let Some(b) = Block::locate(child.id()) {
                let (sx, sy) = cfg.signed_sensitivity();
                b.set_sensitivity(sx, sy);
                b.set_pad_buttons(0);
                b.set_enabled(enabled);
                heartbeat = b.heartbeat();
                heartbeat_at = None;
                pad = 0;
                println!("connected to the mod (pid {})", b.pid);
                ever_connected = true;
                block = Some(b);
            }
        }
        if let Some(b) = &block {
            if now - last_validate >= Duration::from_millis(250) {
                last_validate = now;
                if !b.valid() {
                    println!("game restarted, reconnecting");
                    block = None;
                }
            }
        }
        if block.is_none() && !ever_connected && !hinted_version && now - started >= Duration::from_secs(120) {
            hinted_version = true;
            notice(&shared, "Still no connection to the mod. It needs TOTK 1.4.1 with the mod installed; \
                             an unsupported version logs \"totk-mouse: unsupported game version\" in the emulator log.".into());
        }

        if let Some(b) = &block {
            if now - last_hb_read >= Duration::from_millis(50) {
                last_hb_read = now;
                let hb = b.heartbeat();
                if hb != heartbeat {
                    heartbeat = hb;
                    heartbeat_at = Some(now);
                }
            }
        }
        let camera_active = block.is_some() && heartbeat_at.is_some_and(|t| now - t < ACTIVE_WINDOW);

        // Actions from keys, mouse buttons, the wheel and the UI. Mouse-driven
        // sensitivity changes only count while we hold the mouse, so scrolling on the
        // desktop changes nothing.
        let key_action = |name: &str| bindings.keys.iter().find(|(k, _)| k == name).map(|(_, a)| *a);
        let mut actions: Vec<Action> = batch.keys_pressed.iter().filter_map(|k| key_action(k)).collect();
        let pressed = batch.buttons & !prev_buttons;
        prev_buttons = batch.buttons;
        for (bit, a) in &bindings.mouse_actions {
            if pressed & bit != 0 && (grabbed || *a == Action::Toggle) {
                actions.push(*a);
            }
        }
        if grabbed {
            let wheel = if batch.wheel > 0 { bindings.wheel_up } else { bindings.wheel_down };
            for _ in 0..batch.wheel.unsigned_abs() {
                match wheel {
                    Some(WheelTarget::Action(a)) => actions.push(a),
                    Some(WheelTarget::Pad(bits)) if pulses.len() < PULSE_QUEUE_MAX => pulses.push_back(bits),
                    _ => {}
                }
            }
        }
        if toggle_requested {
            actions.push(Action::Toggle);
        }

        // Held sensitivity bindings repeat.
        let held_sens = batch.keys_held.iter().filter_map(|k| key_action(k))
            .chain(bindings.mouse_actions.iter().filter(|(bit, _)| grabbed && batch.buttons & bit != 0).map(|(_, a)| *a))
            .find(|a| *a != Action::Toggle);
        match (held_sens, repeat_at) {
            (None, _) => repeat_at = None,
            (Some(_), None) => repeat_at = Some(now + REPEAT_DELAY),
            (Some(a), Some(t)) if now >= t => {
                actions.push(a);
                repeat_at = Some(now + REPEAT_EVERY);
            }
            _ => {}
        }

        for a in actions {
            match a {
                Action::Toggle => {
                    enabled = !enabled;
                    if let Some(b) = &block {
                        b.set_enabled(enabled);
                    }
                }
                Action::SensUp | Action::SensDown => {
                    cfg.scale_sensitivity(if a == Action::SensUp { SENS_STEP } else { 1.0 / SENS_STEP });
                    if let Some(b) = &block {
                        let (sx, sy) = cfg.signed_sensitivity();
                        b.set_sensitivity(sx, sy);
                    }
                    println!("sensitivity {:.5} / {:.5}", cfg.sensitivity_x, cfg.sensitivity_y);
                    save_at = Some(now + Duration::from_secs(1));
                }
            }
        }

        if !backend.attached() && last_attach.map_or(true, |t| now - t >= Duration::from_secs(2)) {
            last_attach = Some(now);
            backend.attach(&crate::process::descendants(child.id()));
        }
        camera_since = if camera_active { camera_since.or(Some(now)) } else { None };
        if !hinted_window && !backend.attached() && camera_since.is_some_and(|t| now - t >= Duration::from_secs(10)) {
            hinted_window = true;
            notice(&shared, "The emulator has no X11 window to lock the mouse to (running natively on Wayland?). \
                             Turning still works while it has focus; --input evdev locks the mouse there.".into());
        }

        // Hold-to-open menus (the weapon wheel on Up, for one) stop the camera. Keep
        // the grab, and with it the forwarded button, until mapped buttons are let go,
        // or the menu would close as soon as it opened.
        let holding_pad = grabbed
            && (pulse.is_some() || bindings.pad.iter().any(|(m, _)| batch.buttons & m != 0));
        if holding_pad {
            grace_until = Some(now + HOLD_GRACE);
        }
        let in_grace = grace_until.is_some_and(|t| now < t);
        let want_grab = enabled && (camera_active || holding_pad || (grabbed && in_grace));
        if want_grab {
            if !grabbed || now - last_grab >= REGRAB_EVERY {
                last_grab = now;
                grabbed = backend.set_grab(true);
            }
        } else if grabbed {
            grabbed = backend.set_grab(false);
        }

        if let Some(b) = &mut block {
            // While we hold the mouse the emulator sees none of its buttons, so the
            // mapped ones go through the mod. Otherwise the emulator gets them itself.
            if !grabbed {
                pulses.clear();
                pulse = None;
            }
            if pulse.is_some_and(|(_, _, done)| now >= done) {
                pulse = None;
            }
            if pulse.is_none() {
                pulse = pulses.pop_front().map(|bits| (bits, now + PULSE_PRESS, now + PULSE_PRESS + PULSE_GAP));
            }
            let pulse_bits = pulse.filter(|(_, release, _)| now < *release).map_or(0, |(bits, _, _)| bits);

            let held = batch.buttons & !bindings.action_bits();
            let want_pad = if grabbed {
                bindings.pad.iter().filter(|(m, _)| held & m != 0).fold(pulse_bits, |acc, (_, s)| acc | s)
            } else {
                0
            };
            if want_pad != pad {
                pad = want_pad;
                b.set_pad_buttons(pad);
            }

            if enabled {
                total_x += batch.dx as i64;
                total_y += batch.dy as i64;
            }
            if (total_x, total_y) != written && now - last_write >= WRITE_EVERY {
                written = (total_x, total_y);
                last_write = now;
                b.set_totals(total_x, total_y);
            }
        }

        if save_at.is_some_and(|t| now >= t) {
            save_at = None;
            if let Err(e) = cfg.save() {
                notice(&shared, format!("cannot save the config: {e}"));
            }
        }

        let s = Status { connected: block.is_some(), enabled, camera_active, grabbed };
        if status != Some(s) {
            status = Some(s);
            println!("{}", s.line());
        }
        let mut st = shared.lock().unwrap();
        st.status = s;
        if st.config.sensitivity_x != cfg.sensitivity_x || st.config.sensitivity_y != cfg.sensitivity_y {
            st.config.sensitivity_x = cfg.sensitivity_x;
            st.config.sensitivity_y = cfg.sensitivity_y;
        }
    }
}
