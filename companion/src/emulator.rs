//! Finding the emulator and the game.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};

/// Yuzu forks, most likely first. Each keeps its Qt config in ~/.config/<name>
/// (%APPDATA%\<name>\config on Windows, or a `user` folder next to a portable exe).
const YUZU_FORKS: &[&str] = &["eden", "citron", "citron-neo-nightly", "sudachi", "torzu", "suyu", "yuzu"];
/// Ryujinx and its forks (Ryubing) keep a JSON config in ~/.config/Ryujinx
/// (%APPDATA%\Ryujinx on Windows, or a `portable` folder next to the exe).
const RYUJINX: &[&str] = &["Ryujinx", "ryujinx"];
const EXE_SUFFIX: &str = std::env::consts::EXE_SUFFIX;

const TITLE_ID: &str = "0100F2C0115B6000";

/// The configured emulator, else one next to our own executable (portable installs,
/// common on Windows), else one on PATH.
pub fn find_emulator(configured: &str) -> Option<PathBuf> {
    if !configured.is_empty() {
        return Some(PathBuf::from(configured));
    }
    let own_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf));
    let path = std::env::var_os("PATH").unwrap_or_default();
    let dirs: Vec<PathBuf> = own_dir.into_iter().chain(std::env::split_paths(&path)).collect();
    YUZU_FORKS
        .iter()
        .chain(RYUJINX)
        .find_map(|name| dirs.iter().map(|d| d.join(format!("{name}{EXE_SUFFIX}"))).find(|p| p.is_file()))
}

fn is_ryujinx(emulator: &Path) -> bool {
    let name = emulator.file_stem().and_then(|n| n.to_str()).unwrap_or("");
    name.eq_ignore_ascii_case("ryujinx")
}

/// The emulator's game folders, each with whether to look into subfolders.
fn game_dirs(emulator: &Path) -> Vec<(PathBuf, bool)> {
    if is_ryujinx(emulator) { ryujinx_game_dirs(emulator) } else { yuzu_game_dirs(emulator) }
}

/// Ryujinx scans its game folders recursively.
fn ryujinx_game_dirs(emulator: &Path) -> Vec<(PathBuf, bool)> {
    let mut candidates = Vec::new();
    if let Some(dir) = emulator.parent() {
        candidates.push(dir.join("portable").join("Config.json"));
    }
    #[cfg(windows)]
    if let Some(appdata) = std::env::var_os("APPDATA") {
        candidates.push(PathBuf::from(appdata).join("Ryujinx").join("Config.json"));
    }
    #[cfg(not(windows))]
    candidates.push(PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config/Ryujinx/Config.json"));
    let Some(json) = candidates.into_iter().find_map(|p| std::fs::read_to_string(p).ok()) else { return Vec::new() };
    let Ok(cfg) = serde_json::from_str::<serde_json::Value>(&json) else { return Vec::new() };
    cfg["game_dirs"]
        .as_array()
        .map(|dirs| dirs.iter().filter_map(|d| d.as_str()).map(|d| (PathBuf::from(d), true)).collect())
        .unwrap_or_default()
}

/// The game folders from a yuzu fork's Qt config.
fn yuzu_game_dirs(emulator: &Path) -> Vec<(PathBuf, bool)> {
    let name = emulator.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let config_name = YUZU_FORKS.iter().find(|k| name.starts_with(*k)).map(|k| {
        // citron-neo-nightly still uses citron's config.
        if k.starts_with("citron") { "citron" } else { k }
    });
    let Some(config_name) = config_name else { return Vec::new() };
    let Some(ini) = config_candidates(emulator, config_name).into_iter().find_map(|p| std::fs::read_to_string(p).ok())
    else {
        return Vec::new();
    };
    let mut dirs: Vec<(String, PathBuf, bool)> = Vec::new();
    for line in ini.lines() {
        let Some(rest) = line.strip_prefix("Paths\\gamedirs\\") else { continue };
        let Some((idx, kv)) = rest.split_once('\\') else { continue };
        if let Some(p) = kv.strip_prefix("path=") {
            // Skips the virtual entries (SDMC, UserNAND, SysNAND).
            if Path::new(p).is_absolute() {
                dirs.push((idx.to_string(), PathBuf::from(p), false));
            }
        } else if let Some(v) = kv.strip_prefix("deep_scan=") {
            if let Some(d) = dirs.iter_mut().find(|d| d.0 == idx) {
                d.2 = v == "true";
            }
        }
    }
    dirs.into_iter().map(|(_, p, deep)| (p, deep)).collect()
}

/// Where the emulator's Qt config may live.
fn config_candidates(emulator: &Path, config_name: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    // Portable installs keep everything in a `user` folder next to the executable.
    if let Some(dir) = emulator.parent() {
        out.push(dir.join("user").join("config").join("qt-config.ini"));
    }
    #[cfg(windows)]
    if let Some(appdata) = std::env::var_os("APPDATA") {
        out.push(PathBuf::from(appdata).join(config_name).join("config").join("qt-config.ini"));
    }
    #[cfg(not(windows))]
    {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        out.push(home.join(".config").join(config_name).join("qt-config.ini"));
    }
    out
}

/// Looks for the base game (not the update) in the emulator's game folders.
pub fn find_game(configured: &str, emulator: &Path) -> Option<PathBuf> {
    if !configured.is_empty() {
        return Some(PathBuf::from(configured));
    }
    for (dir, deep) in game_dirs(emulator) {
        if let Some(found) = scan(&dir, if deep { 3 } else { 0 }) {
            return Some(found);
        }
    }
    None
}

fn scan(dir: &Path, depth: u32) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut subdirs = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            subdirs.push(p);
            continue;
        }
        let name = p.file_name()?.to_string_lossy().to_ascii_uppercase();
        let is_image = name.ends_with(".NSP") || name.ends_with(".XCI");
        if is_image && name.contains(TITLE_ID) {
            return Some(p);
        }
    }
    if depth > 0 {
        subdirs.into_iter().find_map(|d| scan(&d, depth - 1))
    } else {
        None
    }
}

/// Starts the emulator. Yuzu forks only run natively on Wayland when explicitly
/// asked to, and our Linux input needs X11 (XWayland), so make sure nobody asked.
pub fn launch(emulator: &Path, game: Option<&Path>) -> std::io::Result<Child> {
    let mut cmd = Command::new(emulator);
    #[cfg(target_os = "linux")]
    cmd.env_remove("I_WANT_A_BROKEN_WAYLAND_UI").env("QT_QPA_PLATFORM", "xcb");
    if let Some(g) = game {
        cmd.arg(g);
    }
    cmd.spawn()
}
