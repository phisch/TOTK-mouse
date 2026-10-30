# How it works

TOTK has no mouse support, and Switch emulators only hand the game a cursor position
or turn the mouse into a stick. Neither feels like a mouse. This project drives the
camera directly from raw mouse motion instead, in two parts:

- **The mod** (`mod/`), an [exlaunch](https://github.com/shadowninja108/exlaunch) code
  mod that runs inside the game. It rotates the gameplay camera and presses Switch
  buttons on request.
- **The companion** (`companion/`), a host program. It starts the emulator, reads raw
  mouse input, locks the cursor to the game and writes everything into the mod's
  shared block in guest memory.

Everything here is specific to **game version 1.4.1** (main build ID
`965EAB9CEB8EB867F747DA772022C95065C9B927`). The mod checks the instruction at its
hook site and refuses to hook anything else.

## The mod

### Camera hook

The gameplay camera calculator at `main+0x47e3a8` fills a struct on its stack:

| Offset | Field |
|---|---|
| 0x00 | eye, 3 floats |
| 0x0c | target, 3 floats |
| 0x18 | up, 3 floats |
| 0x24 | fov (radians) |

It is filled by a call to `main+0x4805cc`. Right after that call returns
(`main+0x47e664`, instruction `ldr x8, [x19, #1520]`, `0xf942fa68`), `x26` points at
the struct. The mod hooks there and replaces the eye's direction around the target
with its own yaw and pitch, keeping the game's distance. Everything the game does
afterwards, including camera collision and pulling in when the camera nears the
floor, then works on our direction. That is why walls and floors behave as in the
original game.

The direction is absolute: it is taken from the game's view when mouselook turns on,
then driven only by the mouse. The game's auto-follow cannot turn it.

How the site was found: hardware watchpoints followed the committed eye backwards
through three copies (`owner+0x78` ← `cam+0x868` ← `cam+0x818` ← this stack struct).
An earlier hook at the final commit (`main+0x65bedc`, inside `main+0x65965c`) also
worked, but ran after collision, so the camera clipped through the floor.

### Buttons

The companion grabs the mouse, so the emulator never sees its buttons. The mod hooks
all five `nn::hid::GetNpadStates` variants the game imports (FullKey, Handheld,
JoyDual, JoyLeft, JoyRight) and ORs the requested `NpadButton` bits into every
returned state for Npad No1 and Handheld.

### Shared block

A 64-byte, 64-aligned struct in the mod's data. The companion finds it by scanning
guest RAM for its 16-byte signature, which is only assembled at runtime so it never
appears whole in the binary.

| Offset | Field | Written by |
|---|---|---|
| 0 | magic `TMOUSE01` | mod |
| 8 | version (6) | mod |
| 12 | cookie `0x5a17c3e9` | mod |
| 16 | total_x, i64, running mouse counts | companion |
| 24 | total_y, i64 | companion |
| 32 | seq, bumped with every totals write | companion |
| 36 | enabled | companion |
| 40 | sens_x, f32 radians per count, negative inverts | companion |
| 44 | heartbeat, bumped per gameplay camera update | mod |
| 48 | applied, bumped per rotated frame | mod |
| 52 | pad_buttons, NpadButton bits | companion |
| 60 | sens_y | companion |

Totals rather than deltas, so a frame the companion misses loses nothing. The
heartbeat only counts the gameplay camera, so it stops at the title screen, in pause
and in menus. The companion uses that to release the mouse.

### Module header

Nintendo's rtld checks the first instruction of every module. Only for `b +8` (or 0,
or an ARM32 branch) does it take the MOD0 offset from +4. Otherwise it reads a pointer
from +8. exlaunch modules started with `b entrypoint`, so rtld read garbage: Ryujinx
faulted, Eden happened to survive. The mod's `crt0.s` now starts with `b +8`, the MOD0
offset, then the branch to the entry point.

### exlaunch quirk

exlaunch's float inline hook context is laid out `[q0..q31][x0..x29]`, the reverse of
what `InlineFloatCtx` declares. The mod reads registers at raw offsets (`RegX`).

## The companion

| Part | Linux | Windows |
|---|---|---|
| Guest memory | `/proc/<pid>/mem`, regions from `smaps` by resident size | `Read/WriteProcessMemory`, regions from `VirtualQueryEx` |
| Raw mouse | XI2 raw events on the root window (default), or evdev | Raw Input with `RIDEV_INPUTSINK` |
| Cursor lock | X pointer grab confined to the emulator window | `ClipCursor` to one pixel in the window centre |
| Window | GPUI | GPUI (needs a Windows build, see below) |

The emulator window is the largest viewable window owned by the emulator's
processes. Ownership comes from `_NET_WM_PID`, or from the X-Resource extension for
windows without it (Ryujinx's Avalonia window).

Writing another process's memory on Linux needs ptrace rights. With
`ptrace_scope = 1` that means being its ancestor, so the companion launches the
emulator itself.

### Why X11 on Linux

Yuzu forks run under XWayland unless told otherwise, and the companion makes sure of
it when launching. Any X client may read raw, unaccelerated motion (XI_RawMotion)
while an XWayland window has focus, and an X pointer grab with `confine_to` is what
XWayland turns into Wayland pointer confinement. That needs no permissions, unlike
evdev (`input` group or a udev rule). XFixes pointer barriers would let the emulator
keep seeing clicks, but XWayland does not enforce them, so the grab it is.

### Grab rules

- Grab while mouselook is on and the heartbeat moved within 400 ms.
- Keep the grab while a mouse button mapped to a Switch button is held, and for one
  second after, so hold-to-open menus (the weapon wheel) neither close nor flicker
  the cursor free.
- Re-assert the grab every second, since XWayland drops confinement when focus moves.

### Writes

Motion is written at most every 4 ms, totals and seq in one write. Under Wine,
`WriteProcessMemory` goes through wineserver and ptrace-stops the target, and writing
on every mouse event froze the game within seconds.

## Things that do not work

- **The emulator's own mouse** (`nn::hid::GetMouseStates` from inside the game): on
  Eden and Ryujinx the deltas are differences of the cursor position in the 1280x720
  panel. They are coarse, carry pointer acceleration and stop at the window edge.
  Eden's mouse panning confines the cursor but cannot recentre it on Wayland.
- **Write-back** into the camera controller's eye (`owner+232`): the controller eases
  toward its own desired direction and rubber-bands back.
- **GPUI on Windows from Linux**: release builds need HLSL shaders precompiled by
  `fxc.exe`, which only exists on Windows. Cross-compiled Windows builds go without
  the `ui` feature; the release's Windows build is made on a Windows CI runner.
- **GPUI under Wine without DXVK**: Wine's own Direct3D 11 lacks what GPUI's
  renderer needs. The companion then runs without its window.

## Porting to another game version

Every address above changes. The research tooling (NSO extraction, disassembly,
gdbstub watchpoints) is not part of this repository. The rough recipe: find the
camera calculator by watchpointing the committed `sead::LookAtCamera` eye (vtable
`main+0x38ee1f8` on 1.4.1, eye doubles at +0x60) and following the copies back, then
update `CameraCalcOffset` and `CameraCalcExpected` in `mod/source/program/main.cpp`.
