//! Settings, persisted as TOML in `$XDG_CONFIG_HOME/totk-mouse/config.toml`
//! (`%APPDATA%\totk-mouse\config.toml` on Windows).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Sensitivity stays within this range, radians per count.
pub const SENS_MIN: f64 = 0.0002;
pub const SENS_MAX: f64 = 0.01;

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Config {
    /// Emulator executable. Empty: use the first one found on PATH.
    pub emulator: String,
    /// Game file. Empty: look through the emulator's game folders, and if nothing
    /// turns up, start the emulator without one.
    pub game: String,
    /// Radians of camera turn per mouse count.
    pub sensitivity_x: f64,
    pub sensitivity_y: f64,
    pub invert_x: bool,
    pub invert_y: bool,
    /// Mouse input (left, right, middle, back, forward, wheel_up, wheel_down) to a
    /// Switch button name or an action, for example `left = "Y"`, `back = "toggle"`.
    pub buttons: BTreeMap<String, String>,
    /// Key name to action, for example `F7 = "toggle"`. Keys reach the emulator as
    /// usual, so only actions are bound here (X11 input only).
    pub keys: BTreeMap<String, String>,
    /// Older configs, converted into `buttons` and `keys` on load.
    #[serde(skip_serializing)]
    toggle_key: Option<String>,
    #[serde(skip_serializing)]
    toggle_button: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Toggle,
    SensUp,
    SensDown,
}

impl Action {
    pub const ALL: [Action; 3] = [Action::Toggle, Action::SensUp, Action::SensDown];

    pub fn parse(s: &str) -> Option<Action> {
        match s.to_ascii_lowercase().as_str() {
            "toggle" | "toggle_mouselook" => Some(Action::Toggle),
            "sensitivity_up" => Some(Action::SensUp),
            "sensitivity_down" => Some(Action::SensDown),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Action::Toggle => "toggle",
            Action::SensUp => "sensitivity_up",
            Action::SensDown => "sensitivity_down",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Action::Toggle => "Toggle mouselook",
            Action::SensUp => "Sensitivity +",
            Action::SensDown => "Sensitivity -",
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            emulator: String::new(),
            game: String::new(),
            sensitivity_x: 0.0006,
            sensitivity_y: 0.0006,
            invert_x: false,
            invert_y: false,
            buttons: BTreeMap::from([
                ("left".into(), "Y".into()),
                ("right".into(), "ZR".into()),
                ("back".into(), "toggle".into()),
                ("wheel_up".into(), "sensitivity_up".into()),
                ("wheel_down".into(), "sensitivity_down".into()),
            ]),
            keys: BTreeMap::from([("F7".into(), "toggle".into())]),
            toggle_key: None,
            toggle_button: None,
        }
    }
}

pub fn path() -> PathBuf {
    #[cfg(windows)]
    let base = PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default());
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"));
    base.join("totk-mouse").join("config.toml")
}

impl Config {
    /// Loads the config, writing the defaults first if there is none yet so users
    /// have a file to edit.
    pub fn load() -> Result<Self, String> {
        let p = path();
        match std::fs::read_to_string(&p) {
            Ok(s) => {
                let mut c: Config = toml::from_str(&s).map_err(|e| format!("{}: {e}", p.display()))?;
                c.sensitivity_x = c.sensitivity_x.clamp(SENS_MIN, SENS_MAX);
                c.sensitivity_y = c.sensitivity_y.clamp(SENS_MIN, SENS_MAX);
                if c.migrate() {
                    c.save()?;
                }
                Ok(c)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let c = Config::default();
                c.save()?;
                Ok(c)
            }
            Err(e) => Err(format!("{}: {e}", p.display())),
        }
    }

    /// Converts the old toggle_key/toggle_button fields. The wheel used to always
    /// adjust sensitivity, so such configs get that as explicit bindings.
    fn migrate(&mut self) -> bool {
        if self.toggle_key.is_none() && self.toggle_button.is_none() {
            return false;
        }
        if let Some(k) = self.toggle_key.take().filter(|k| !k.is_empty()) {
            self.keys.insert(k, "toggle".into());
        }
        if let Some(b) = self.toggle_button.take().filter(|b| !b.is_empty()) {
            self.buttons.insert(b, "toggle".into());
        }
        self.buttons.entry("wheel_up".into()).or_insert_with(|| "sensitivity_up".into());
        self.buttons.entry("wheel_down".into()).or_insert_with(|| "sensitivity_down".into());
        true
    }

    /// The key bound to `action`, if any.
    pub fn key_for(&self, action: Action) -> Option<&str> {
        self.keys.iter().find(|(_, a)| Action::parse(a) == Some(action)).map(|(k, _)| k.as_str())
    }

    /// Binds `key` to `action` (or unbinds the action when `key` is None), keeping
    /// one key per action and one action per key.
    pub fn bind_key(&mut self, action: Action, key: Option<String>) {
        self.keys.retain(|_, a| Action::parse(a) != Some(action));
        if let Some(k) = key {
            self.keys.insert(k, action.name().into());
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let p = path();
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let s = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        // Write and rename, so a crash never leaves a truncated config behind.
        let tmp = p.with_extension("toml.tmp");
        std::fs::write(&tmp, s).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &p).map_err(|e| format!("{}: {e}", p.display()))
    }

    /// Sensitivity as the mod wants it: inversion is a negative value.
    pub fn signed_sensitivity(&self) -> (f32, f32) {
        let x = if self.invert_x { -self.sensitivity_x } else { self.sensitivity_x };
        let y = if self.invert_y { -self.sensitivity_y } else { self.sensitivity_y };
        (x as f32, y as f32)
    }

    /// Scales both axes together. The factor is clamped so the faster axis stops at
    /// SENS_MAX and the slower at SENS_MIN, which keeps their ratio.
    pub fn scale_sensitivity(&mut self, factor: f64) {
        let (x, y) = (self.sensitivity_x, self.sensitivity_y);
        let f = factor.clamp(SENS_MIN / x.min(y), SENS_MAX / x.max(y));
        // Rounded to 7 decimals so the saved config stays readable.
        let round = |v: f64| (v * 1e7).round() / 1e7;
        self.sensitivity_x = round(x * f);
        self.sensitivity_y = round(y * f);
    }
}

/// Mouse buttons as bits in `input::Batch::buttons`. The wheel is not a button,
/// see `Bindings::wheel_up`.
pub fn mouse_button_bit(name: &str) -> Option<u32> {
    Some(match name.to_ascii_lowercase().as_str() {
        "left" => 1 << 0,
        "right" => 1 << 1,
        "middle" => 1 << 2,
        "back" => 1 << 3,
        "forward" => 1 << 4,
        _ => return None,
    })
}

/// `nn::hid::NpadButton` bits.
pub fn switch_button_bit(name: &str) -> Option<u32> {
    let bit = match name.to_ascii_uppercase().as_str() {
        "A" => 0,
        "B" => 1,
        "X" => 2,
        "Y" => 3,
        "LSTICK" | "L3" => 4,
        "RSTICK" | "R3" => 5,
        "L" => 6,
        "R" => 7,
        "ZL" => 8,
        "ZR" => 9,
        "PLUS" | "+" => 10,
        "MINUS" | "-" => 11,
        "LEFT" => 12,
        "UP" => 13,
        "RIGHT" => 14,
        "DOWN" => 15,
        _ => return None,
    };
    Some(1 << bit)
}

/// What a wheel notch does. A Switch button is pressed briefly per notch.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum WheelTarget {
    Action(Action),
    Pad(u32),
}

/// The config's bindings, resolved for the session loop.
#[derive(Default)]
pub struct Bindings {
    /// (mouse bit, Switch bits) held while the mouse button is held.
    pub pad: Vec<(u32, u32)>,
    /// (mouse bit, action) triggered on press.
    pub mouse_actions: Vec<(u32, Action)>,
    pub wheel_up: Option<WheelTarget>,
    pub wheel_down: Option<WheelTarget>,
    /// (key name, action).
    pub keys: Vec<(String, Action)>,
}

impl Bindings {
    /// Unknown names are reported and skipped.
    pub fn resolve(cfg: &Config) -> Bindings {
        let mut b = Bindings::default();
        for (input, target) in &cfg.buttons {
            let action = Action::parse(target);
            let wheel = action.map(WheelTarget::Action).or_else(|| switch_button_bit(target).map(WheelTarget::Pad));
            match (input.as_str(), wheel) {
                ("wheel_up", Some(w)) => b.wheel_up = Some(w),
                ("wheel_down", Some(w)) => b.wheel_down = Some(w),
                _ => match (mouse_button_bit(input), action, switch_button_bit(target)) {
                    (Some(m), Some(a), _) => b.mouse_actions.push((m, a)),
                    (Some(m), None, Some(s)) => b.pad.push((m, s)),
                    _ => eprintln!("config: ignoring binding {input} = {target:?}"),
                },
            }
        }
        for (key, target) in &cfg.keys {
            match Action::parse(target) {
                Some(a) => b.keys.push((key.clone(), a)),
                None => eprintln!("config: ignoring key binding {key} = {target:?}"),
            }
        }
        b
    }

    /// Mouse buttons bound to actions, which never reach the game.
    pub fn action_bits(&self) -> u32 {
        self.mouse_actions.iter().fold(0, |acc, (m, _)| acc | m)
    }
}
