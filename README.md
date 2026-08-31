# Building Hebnix (Linux)

This is a Linux port of [Hebnix](https://hebnix.com), targeting **Hyprland**
(or another wlroots-based Wayland compositor with `wlr-layer-shell` support).
KDE Plasma/KWin has a best-effort backend for window focus/geometry but has
not been tested against a real Plasma session — see [Known limitations](#known-limitations).

## Requirements

- **Rust** (stable), via [rustup](https://rustup.rs)
- A C compiler (`gcc`/`clang`) — needed to build vendored Lua and a couple of
  other native dependencies
- System libraries: GTK3, Wayland client headers, and an app-indicator
  library (for the system tray icon)

### Arch Linux

```sh
sudo pacman -S --needed base-devel gtk3 libayatana-appindicator wayland libxkbcommon
```

### Debian / Ubuntu

```sh
sudo apt install build-essential pkg-config libgtk-3-dev libayatana-appindicator3-dev \
  libwayland-dev libxkbcommon-dev
```

### Fedora

```sh
sudo dnf install gcc gtk3-devel libappindicator-gtk3-devel wayland-devel libxkbcommon-devel
```

## Build

```sh
git clone https://github.com/xplodingeggo/hebnix-linux.git
cd hebnix-linux
cargo build --release
```

The binary lands at `target/release/hebnix-app`. It reads its config,
plugin, and cache directories from wherever it's actually run from (next to
the executable) — no install step needed, just run it in place:

```sh
./target/release/hebnix-app
```

On first run it creates an empty `plugins/` folder next to itself. Plugins
aren't bundled in this repo; install them either through the app's own
Plugins tab, or by cloning a plugin repo (e.g.
[`rl-profiles-linux`](https://github.com/xplodingeggo/rl-profiles-linux))
into that `plugins/` folder yourself.

## Optional: hotkeys, binds & chat-send plugins

Reading key/controller state (the show/hide hotkey, `hebnix.is_bind_pressed`,
etc) goes through `/dev/input/event*`, which needs your user in the `input`
group:

```sh
sudo usermod -aG input $USER
# then log out and back in (or reboot) for the new group to apply
```

Plugins that *send* synthetic input (`hebnix.input.send`, `hebnix.chat.send`
— e.g. quick-chat plugins) additionally need a virtual keyboard via
`/dev/uinput`, which isn't group-`input`-writable by default on most distros
(unlike `/dev/input/event*`, which already is via systemd's own udev rules).
One-time setup:

```sh
echo uinput | sudo tee /etc/modules-load.d/uinput.conf
sudo modprobe uinput
echo 'KERNEL=="uinput", GROUP="input", MODE="0660", OPTIONS+="static_node=uinput"' \
  | sudo tee /etc/udev/rules.d/60-hebnix-uinput.rules
sudo udevadm control --reload-rules
sudo udevadm trigger /dev/uinput
```

`install.sh` checks both of these and offers to set them up for you. Neither
is a hard requirement — without them the app runs fine, hotkeys/binds just
read as "not pressed" and chat-send plugins fail to type until it's fixed.

## Controllers

`hebnix.controllers()` reports every connected gamepad through a generic
SDL-style mapping (`kind = "universal"`, `btn_south`/`btn_east`/`dpad_*`/
`lx`/`ly`/etc) — this is the same fallback path the Windows build uses for
anything that isn't a real Xbox controller (what Windows calls a DirectInput
device), so DInput-style pads already just work here with no extra code:
Linux's evdev/joystick layer doesn't distinguish XInput from DirectInput at
the OS level the way Windows does, one generic path covers both. Only real
Xbox controllers get a narrower Windows-only `kind = "xinput"` fast path with
the raw `XINPUT_*` fields; on Linux they report as `"universal"` too and are
still fully readable through the same fields plugins already use for any
other pad.

## Optional: avatar/tracker.gg lookups

Plugins that fetch player stats or avatars from tracker.gg need
[`curl-impersonate`](https://github.com/lexiforest/curl-impersonate) (the
site blocks by TLS fingerprint, so a plain HTTP client gets rejected).
Download a prebuilt Linux release and place the `curl-impersonate` binary
at:

```
<next to hebnix-app>/curl-impersonate/curl-impersonate
```

Without it, stats/avatar fetches just fail gracefully — everything else
works fine.

## Known limitations

- **KDE/Plasma**: window focus tracking, popping the app over a fullscreened
  game, and monitor geometry go through [`kdotool`](https://github.com/jinliu/kdotool)
  (AUR: `kdotool-bin`) and `kscreen-doctor` instead of Hyprland's IPC socket.
  This was written against those tools' own source/docs but **has not been
  run on a real Plasma session** — if you hit issues there, please open one.
- **Other Wayland compositors** (Sway, etc.) get the in-game overlay (if
  `wlr-layer-shell` is supported) but no window focus/geometry tracking —
  the app falls back to "always focused" so binds/overlays don't just go
  dead, but features like popping the window over a fullscreened game won't
  work.
- Play Rocket League in **real fullscreen** (F11 in-game), not Borderless
  Windowed — it works either way, but real fullscreen gets you the game
  overlay *and* Hyprland's native auto-hide-your-bar-during-fullscreen
  behavior for free.
