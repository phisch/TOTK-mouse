# TOTK Mouse

> [!CAUTION]
> **This is AI slop.** This repository was created with AI (Anthropic's Claude), with a
> human directing, testing and reviewing. It is not a showcase of anyone's skill.
>
> At least 99% of the credit belongs to the people whose work all of this stands on:
>
> - [exlaunch](https://github.com/shadowninja108/exlaunch) by shadowninja108, the code
>   injection framework the mod is built on
> - [libnx](https://github.com/switchbrew/libnx) by the switchbrew team, parts of which
>   exlaunch bundles
> - [Atmosphère](https://github.com/Atmosphere-NX/Atmosphere) and
>   [oss-rtld](https://github.com/Thog/oss-rtld) by Thog, which exlaunch builds on
> - [Eden](https://git.eden-emu.dev/eden-emu/eden), [Citron](https://citron-emu.org)
>   and the yuzu project they descend from, without which neither running nor
>   debugging the game would be possible
> - [devkitPro](https://devkitpro.org), whose toolchain builds the mod
> - [GPUI](https://github.com/zed-industries/zed) by Zed Industries and
>   [x11rb](https://github.com/psychon/x11rb), which the companion uses
> - [totk-mousecam](https://github.com/mcrib884/totk-mousecam), which showed that a
>   mouse camera mod for TOTK is possible
>
> Treat the code accordingly.

Raw mouse support for The Legend of Zelda: Tears of the Kingdom on Switch emulators.
The mouse drives the camera 1:1, the game's own camera collision keeps working, the
cursor is locked to the game while you play, and mouse buttons can be bound to Switch
buttons.

It has two parts: a game mod (`mod/`) and a companion app (`companion/`) that feeds it
the mouse.

## Requirements

- TOTK version **1.4.1**. Other versions are refused, nothing gets patched.
- Eden or Citron (other yuzu forks likely work).
- Linux with X11 or XWayland. Windows works under Wine, untested on real Windows.

## Usage

Download the archive for your system from the
[latest release](https://github.com/phisch/TOTK-mouse/releases/latest) and extract it.

1. **Install the mod.** Copy the `mod/totk-mouse` folder into the emulator's mod folder
   for TOTK. In Eden or Citron: right-click the game, then "Open Mod Data Location".
   On Linux that is usually `~/.local/share/eden/load/0100F2C0115B6000/`.
2. **Start the companion** instead of the emulator. It starts the emulator with the
   game.
   - Linux: run `./totk-mouse`. The emulator has to be on your PATH (`eden`,
     `citron`, ...).
   - Windows: put `totk-mouse.exe` next to the emulator's executable (for example
     next to `eden.exe`) and run it.

   If the emulator is somewhere else, set `emulator` in the config file
   (`~/.config/totk-mouse/config.toml`, or `%APPDATA%\totk-mouse\config.toml` on
   Windows), which is created on first start.
3. **Load your save.** The mouse turns the camera once you are in the world. Change
   bindings and sensitivity in the companion window. Closing it turns mouse support
   off.

The game is found through the emulator's game folders. If that fails, start it from the
emulator window.

## Building from source

Mod (Docker):

```sh
cd mod
docker run --rm -v "$PWD":/mod -w /mod --user $(id -u):$(id -g) devkitpro/devkita64 \
  bash -lc 'export DEVKITPRO=/opt/devkitpro; make -j'
```

Companion:

```sh
cd companion
cargo build --release
```

On Windows, `cargo build --release` builds the same with the window. Cross-compiling
from Linux (`--target x86_64-pc-windows-gnu --no-default-features`) gives a terminal-only
build, because GPUI needs the Windows shader compiler.

How it works: [docs/how-it-works.md](docs/how-it-works.md).

## License

- Mod: GPL-2.0, as required by exlaunch. See `mod/LICENSE` and `mod/NOTICE`.
- Companion: GPL-3.0, see `companion/LICENSE`. Bundled fonts are under the SIL Open
  Font License.
