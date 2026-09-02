# Hebnix (Linux)

This is a Linux port of [Hebnix](https://hebnix.com), targeting **Hyprland**
(or another wlroots-based Wayland compositor with `wlr-layer-shell` support).
KDE Plasma/KWin has a best-effort backend for window focus/geometry but has
not been tested against a real Plasma session — see [Known limitations](#known-limitations).

## Installing

### Arch Linux

- **[`hebnix-linux-bin`](https://aur.archlinux.org/packages/hebnix-linux-bin)**
  (AUR) — recommended for most users. Installs the prebuilt upstream
  binary, no Rust toolchain or compile time needed.
- **[`hebnix-linux`](https://aur.archlinux.org/packages/hebnix-linux)** (AUR)
  — builds from source, for anyone who wants to build against their own
  toolchain or track a specific release.

```sh
yay -S hebnix-linux-bin   # or: yay -S hebnix-linux
```

Either way pacman/yay/paru owns updates — Hebnix does not update or
overwrite `/usr/bin/hebnix` itself.

### Other distros

- **AppImage** (recommended) — download the latest `Hebnix-*-x86_64.AppImage`
  from the [releases page](https://github.com/xplodingeggo/hebnix-linux/releases),
  `chmod +x` it, and run it. No install step, works on most distros.
- **Build from source** (developers/advanced users) — see
  [Building from source](#building-from-source) below.

### Development / nightly builds

Bleeding-edge builds from `main` are published as the rolling
[`nightly` prerelease](https://github.com/xplodingeggo/hebnix-linux/releases/tag/nightly)
(tarball + AppImage). These are development builds, not stable releases —
package-manager and stable-AppImage installs are never silently upgraded
onto them.

## Where your data lives

Config, plugins, and themes are managed by Hebnix itself at
`$XDG_CONFIG_HOME/hebnix` (falling back to `~/.config/hebnix` if that's
unset) — never inside a read-only package or AppImage, and never touched by
installing, upgrading, or removing any of the above. Installs from before
this layout (config used to live next to the executable) are migrated
automatically the first time you run the new version.

## Building from source

### Requirements

- **Rust** (stable), via [rustup](https://rustup.rs)
- A C compiler (`gcc`/`clang`) — needed to build vendored Lua and a couple of
  other native dependencies
- System libraries: GTK3, Wayland client headers, and an app-indicator
  library (for the system tray icon)

### Arch Linux

```sh
sudo pacman -S --needed base-devel gtk3 libayatana-appindicator wayland libxkbcommon \
  systemd-libs alsa-lib openssl xdotool libx11 libxtst libxi webkit2gtk-4.1 libsoup3 gtk-layer-shell
```

### Debian / Ubuntu

```sh
sudo apt install build-essential pkg-config libgtk-3-dev libayatana-appindicator3-dev \
  libwayland-dev libxkbcommon-dev libudev-dev libasound2-dev libssl-dev libxdo-dev \
  libx11-dev libxtst-dev libxi-dev libwebkit2gtk-4.1-dev libsoup-3.0-dev libgtk-layer-shell-dev
```

### Fedora

```sh
sudo dnf install gcc gtk3-devel libappindicator-gtk3-devel wayland-devel libxkbcommon-devel \
  systemd-devel alsa-lib-devel openssl-devel libxdo-devel libX11-devel libXtst-devel \
  libXi-devel webkit2gtk4.1-devel libsoup3-devel gtk-layer-shell-devel
```

(`install.sh` below checks all of these for you and prints the right command
for your distro if anything's missing.)

## Build & install

```sh
git clone https://github.com/xplodingeggo/hebnix-linux.git
cd hebnix-linux
./install.sh
```

`install.sh` checks dependencies, offers to set up optional hotkey/uinput
device access (see below), then builds and installs via `make install` —
the same canonical build (`make release`) and install (`make DESTDIR=...
PREFIX=... install`) path used by CI, the AppImage, and the AUR packages.
It installs the `hebnix` command to `~/.local/bin`, plus a `.desktop` entry
and icon, no sudo needed for any of that. Just building without installing:

```sh
make release       # binary lands at target/release/hebnix-app
make install        # -> ~/.local/bin/hebnix (override with PREFIX=/some/where)
```

Run it with `hebnix` (make sure `~/.local/bin` is on your `PATH`) or from
your application menu. On first run it creates an empty `plugins/` folder
under `$XDG_CONFIG_HOME/hebnix` (see [Where your data lives](#where-your-data-lives)).
Plugins aren't bundled in this repo; install them either through the app's
own Plugins tab, or by cloning a plugin repo (e.g.
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

**AUR installs** (`hebnix-linux`/`hebnix-linux-bin`) already ship the
`uinput` module-load config and udev rule as package-owned files — you only
need the one-time group membership step above.

**Manual/source installs**: `install.sh` checks for both and offers to set
them up interactively (module + udev rule need sudo once). To do it by
hand instead:

```sh
echo uinput | sudo tee /etc/modules-load.d/uinput.conf
sudo modprobe uinput
echo 'KERNEL=="uinput", GROUP="input", MODE="0660", OPTIONS+="static_node=uinput"' \
  | sudo tee /etc/udev/rules.d/60-hebnix-uinput.rules
sudo udevadm control --reload-rules
sudo udevadm trigger /dev/uinput
```

Neither of these is a hard requirement — without them the app runs fine,
hotkeys/binds just read as "not pressed" and chat-send plugins fail to type
until it's fixed.

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
