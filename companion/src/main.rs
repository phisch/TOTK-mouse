//! Companion for the totk-mouse mod: launches the emulator, reads raw mouse input and
//! feeds it to the mod through its shared block in guest memory.

// Without the window, the parts only it uses are unused.
#![cfg_attr(not(feature = "ui"), allow(dead_code))]
// The Windows build with a window should not also open a console.
#![cfg_attr(all(windows, feature = "ui"), windows_subsystem = "windows")]

mod config;
mod emulator;
mod guest;
mod input;
mod process;
mod session;
#[cfg(feature = "ui")]
mod ui;

use config::Config;
use input::Backend;
use session::{Shared, State, Status};
use std::sync::{mpsc, Arc, Mutex};

#[cfg(target_os = "linux")]
const DEFAULT_INPUT: &str = "x11";
#[cfg(windows)]
const DEFAULT_INPUT: &str = "rawinput";

struct Args {
    input: String,
    emulator: Option<String>,
    game: Option<String>,
    ui: bool,
}

fn parse_args() -> Args {
    let mut a = Args { input: DEFAULT_INPUT.into(), emulator: None, game: None, ui: cfg!(feature = "ui") };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--input" => a.input = it.next().expect("--input needs a backend name"),
            "--emulator" => a.emulator = Some(it.next().expect("--emulator needs a path")),
            "--game" => a.game = Some(it.next().expect("--game needs a path")),
            "--no-ui" => a.ui = false,
            "-h" | "--help" => {
                println!("totk-mouse [--input BACKEND] [--emulator PATH] [--game PATH] [--no-ui]");
                println!("Input backends: x11 (default) or evdev on Linux, rawinput on Windows.");
                println!();
                println!("Starts the emulator with TOTK and drives the camera with the mouse.");
                println!("Settings live in {}.", config::path().display());
                println!("The mouse wheel scales sensitivity while mouselook is on; it is saved.");
                std::process::exit(0);
            }
            other => {
                eprintln!("unknown argument {other} (see --help)");
                std::process::exit(2);
            }
        }
    }
    a
}

fn fail(msg: impl std::fmt::Display) -> ! {
    eprintln!("error: {msg}");
    // Without a console nobody would see the message.
    #[cfg(all(windows, feature = "ui"))]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
        let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
        let (text, title) = (wide(&msg.to_string()), wide("TOTK Mouse"));
        // SAFETY: both strings are NUL terminated and outlive the call.
        unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), MB_OK | MB_ICONERROR) };
    }
    std::process::exit(1);
}

fn main() {
    let args = parse_args();
    let cfg = Config::load().unwrap_or_else(|e| fail(format!("config: {e}")));

    let keys: Vec<String> = cfg.keys.keys().cloned().collect();
    let backend: Box<dyn Backend> = match args.input.as_str() {
        #[cfg(target_os = "linux")]
        "x11" => match input::x11::X11::new(&keys) {
            Ok(b) => Box::new(b),
            Err(e) => fail(format!("{e}\nIf you are not running X11 or XWayland, try --input evdev.")),
        },
        #[cfg(target_os = "linux")]
        "evdev" => Box::new(input::evdev::Evdev::new().unwrap_or_else(|e| fail(e))),
        #[cfg(windows)]
        "rawinput" => Box::new(input::rawinput::RawInput::new(&keys).unwrap_or_else(|e| fail(e))),
        other => fail(format!("unknown input backend {other} (see --help)")),
    };

    let emulator = emulator::find_emulator(args.emulator.as_deref().unwrap_or(&cfg.emulator))
        .unwrap_or_else(|| {
            fail(format!(
                "No emulator found. Put totk-mouse next to the emulator's executable, or set `emulator` in {}.",
                config::path().display()
            ))
        });
    let game = emulator::find_game(args.game.as_deref().unwrap_or(&cfg.game), &emulator);
    match &game {
        Some(g) => println!("game: {}", g.display()),
        None => println!("game not found in the emulator's game folders; start it from the emulator"),
    }
    let child = emulator::launch(&emulator, game.as_deref())
        .unwrap_or_else(|e| fail(format!("cannot launch {}: {e}", emulator.display())));
    println!("started {} (input: {})", emulator.display(), backend.name());

    let shared: Shared = Arc::new(Mutex::new(State {
        config: cfg.clone(),
        status: Status::default(),
        emulator: emulator.display().to_string(),
        input: backend.name(),
        notices: Vec::new(),
        finished: false,
    }));
    let (tx, rx) = mpsc::channel();
    let session = {
        let shared = shared.clone();
        std::thread::spawn(move || session::run(child, backend, cfg, shared, rx))
    };

    #[cfg(feature = "ui")]
    if args.ui {
        ui::run(shared, tx, session);
        return;
    }
    drop((shared, tx));
    let _ = session.join();
}
