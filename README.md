# TOTK Mouse

Real mouse support for The Legend of Zelda: Tears of the Kingdom on Switch emulators.
Raw 1:1 mouselook, not a mouse-to-stick translation. The game's own camera collision
keeps working, the cursor is locked to the game while you play, and mouse buttons can
be bound to Switch buttons.

It consists of a small game mod and a companion app that feeds it the mouse.

## Requirements

- TOTK **version 1.4.1**. Other versions are refused, nothing is patched.
- A yuzu based emulator: Eden (tested), Citron (tested), others likely.
- Linux with X11 or XWayland. Windows support works under Wine and is not yet tested
  on real Windows.

## Quick start (Linux)

1. Build the mod (see below) and copy `mod/deploy/totk-mouse` into your emulator's mod
   folder for TOTK, for example
   `~/.local/share/eden/load/0100F2C0115B6000/totk-mouse`.
2. Build and run the companion:
   ```sh
   cd companion
   cargo run --release
   ```
   It finds the emulator on your PATH and the game in the emulator's game folders, then
   starts both.
3. Load your save. The mouse turns the camera as soon as you are in the world.

Use the companion window to change sensitivity, inversion, shortcuts and mouse button
bindings. Settings are saved to `~/.config/totk-mouse/config.toml`
(`%APPDATA%\totk-mouse\config.toml` on Windows).

| Default binding | Action |
|---|---|
| F7 | Toggle mouselook |
| Back mouse button | Toggle mouselook |
| Wheel up / down | Sensitivity + / - |
| Left click | Y (attack) |
| Right click | ZR (bow) |

The mouse is released automatically in pause and menus. Closing the companion window
turns mouse support off.

## Building

### Mod

Needs Docker (or a local devkitA64):

```sh
cd mod
docker run --rm -v "$PWD":/mod -w /mod --user $(id -u):$(id -g) devkitpro/devkita64 \
  bash -lc 'export DEVKITPRO=/opt/devkitpro; make -j'
```

The result is `mod/deploy/totk-mouse/exefs/subsdk8`.

### Companion

```sh
cd companion
cargo build --release
```

Options: `--emulator PATH`, `--game PATH`, `--no-ui`, and `--input evdev` for setups
without X11 (needs read access to `/dev/input`).

Windows (cross-compiled, terminal only for now):

```sh
cargo build --release --target x86_64-pc-windows-gnu --no-default-features
```

## How it works

See [docs/how-it-works.md](docs/how-it-works.md).

## License and credits

- The mod is built on [exlaunch](https://github.com/shadowninja108/exlaunch) by
  shadowninja108 and is distributed under the GPL-2.0. See `mod/LICENSE` and
  `mod/NOTICE` for the base version and what was changed.
- `mod/source/rtld` is by Thog under its own license, see
  `mod/source/rtld/LICENSE.txt`.
- The companion bundles IBM Plex Sans and Lilex under the SIL Open Font License, see
  `companion/assets/fonts`.
