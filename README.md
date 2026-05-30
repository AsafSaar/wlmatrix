# wlmatrix

A native **Wayland** Matrix digital-rain screensaver for GNOME, rendered entirely
on the **CPU** via `wl_shm` (software framebuffer) — **no OpenGL, no GPU, no
XWayland, no terminal**.

It exists because GNOME on Wayland removed animated screensavers, and on this
machine (4K display + NVIDIA + XWayland) *every* conventional option failed to
render fullscreen. Going native + CPU-only was the only reliable path. See
[Why this exists](#why-this-exists) for the full story.

![status](https://img.shields.io/badge/status-working-brightgreen)

---

## What it does

- Fills the screen with falling green "Matrix" rain — bright white leaders,
  green trails that fade out.
- True fullscreen as a real Wayland surface (no resize/compositor glitches).
- Dismisses instantly on any keypress, mouse movement, or click.
- Hidden mouse cursor while running (it's a fullscreen surface that exits the
  moment you move).

The screensaver itself is just the renderer. A small companion daemon
(`wl-screensaver`) decides *when* to show it, using GNOME/Mutter's idle timer.

---

## Architecture

Three pieces work together:

| Component | Location | Role |
|-----------|----------|------|
| **`wlmatrix`** | `~/wlmatrix/` (this repo) → binary at `~/.local/bin/wlmatrix` | The renderer. Native Rust/Wayland app, CPU-rendered Matrix rain. Runs fullscreen, exits on input. |
| **`wl-screensaver`** | `~/.local/bin/wl-screensaver` | Idle daemon (bash). Polls Mutter's idle time; launches the renderer after N ms idle and kills it when you return. |
| **`wl-screensaver.service`** | `~/.config/systemd/user/wl-screensaver.service` | systemd user unit. Runs the daemon at login. |

```
 user goes idle
       │
       ▼
 wl-screensaver (daemon)  ──polls──▶  org.gnome.Mutter.IdleMonitor (D-Bus)
       │ idle ≥ SAVER_IDLE_MS
       ▼
 wlmatrix  ──renders──▶  wl_shm framebuffer  ──▶  Wayland compositor (fullscreen)
       │ any input → app exits / daemon kills it
       ▼
 back to desktop
```

### The renderer (`src/main.rs`)

- **`winit`** — creates a native Wayland window and puts it fullscreen
  (`Fullscreen::Borderless`), delivers input events.
- **`softbuffer`** — gives a raw `&mut [u32]` framebuffer backed by `wl_shm`;
  we write `0x00RRGGBB` pixels and `present()`. No GPU involved.
- **`ab_glyph`** — rasterizes monospace glyphs once into a coverage cache; each
  frame we blit cached glyphs in the right color/brightness.
- A tiny built-in xorshift RNG (no `rand` dependency).
- ~30 fps loop driven by `ControlFlow::WaitUntil`.

---

## Requirements

- A Wayland session (GNOME tested). `echo $XDG_SESSION_TYPE` should say `wayland`.
- Rust toolchain (`cargo`).
- A monospace TTF. The app searches, in order:
  `DejaVuSansMono`, `LiberationMono-Regular`, `NotoSansMono-Regular`,
  `NotoMono-Regular` under `/usr/share/fonts`.
- For the daemon/service: `gdbus` (ships with glib) and a systemd user session.

---

## Build & install

The one-liner does everything — builds the binary, installs the renderer + idle
daemon, and enables the user service:

```bash
git clone https://github.com/AsafSaar/wlmatrix
cd wlmatrix
./install.sh
```

That's it; the screensaver kicks in after the idle timeout (default 5 min).

<details>
<summary>Manual steps (what <code>install.sh</code> does)</summary>

```bash
# 1. Build the renderer
cargo build --release

# 2. Install binary (symlink), daemon, and service
mkdir -p ~/.local/bin ~/.config/systemd/user
ln -sf "$PWD/target/release/wlmatrix" ~/.local/bin/wlmatrix
install -m 0755 dist/wl-screensaver          ~/.local/bin/wl-screensaver
install -m 0644 dist/wl-screensaver.service  ~/.config/systemd/user/wl-screensaver.service

# 3. Enable the user service
systemctl --user daemon-reload
systemctl --user enable --now wl-screensaver.service
```
</details>

Run the renderer directly to preview it (grabs the whole screen; move the mouse
to quit):

```bash
~/.local/bin/wlmatrix
```

Check the service: `systemctl --user status wl-screensaver.service`

---

## Usage & configuration

### Idle timeout

Set by `SAVER_IDLE_MS` (milliseconds) in the service file. Default `300000` = 5 min.

```ini
Environment=SAVER_IDLE_MS=300000   # 60000=1m, 180000=3m, 600000=10m
```

After editing: `systemctl --user daemon-reload && systemctl --user restart wl-screensaver`.

### Quick test without waiting

```bash
SAVER_IDLE_MS=5000 ~/.local/bin/wl-screensaver   # 5-second idle; Ctrl-C to stop
```

### Use a different renderer

The daemon runs whatever `SAVER_CMD` points to (default: this app). Any
fullscreen Wayland command works:

```ini
Environment=SAVER_CMD=/home/asaf/.local/bin/wlmatrix
```

### Tuning the look

Edit `src/main.rs`, then rebuild (`cargo build --release …`):

| Constant / code | Effect |
|-----------------|--------|
| `FPS` (30) | animation frame rate |
| `FONT_PX` (26.0) | character size → rain density |
| `6.0 + … * 18.0` in `step()` | fall-speed range (rows/sec) |
| colors in `render()` | head is white `(220,255,220)`, trail green |
| `CHARSET` | which glyphs rain |

---

## Repository layout

```
wlmatrix/
├── src/main.rs              # the renderer
├── Cargo.toml
├── install.sh               # build + install + enable everything
├── dist/
│   ├── wl-screensaver         # idle daemon (bash) → ~/.local/bin/
│   └── wl-screensaver.service # systemd user unit → ~/.config/systemd/user/
├── README.md
└── LICENSE
```

> Idle-parsing note (in `dist/wl-screensaver`): the Mutter idle value is parsed
> with `sed -E 's/.*uint64 ([0-9]+).*/\1/'`, **not** `grep -oE '[0-9]+'` — the
> latter grabs the `64` from `uint64`.

---

## Troubleshooting

- **Nothing happens on idle.** Is the service running? `systemctl --user status wl-screensaver`. Is the binary on PATH? `command -v wlmatrix`. Watch logs: `journalctl --user -u wl-screensaver -f`.
- **It flashes and disappears.** The renderer exited immediately. Run it directly to see errors: `~/.local/bin/wlmatrix`. (It also ignores input for the first 700 ms so the startup event doesn't dismiss it.)
- **Font looks wrong / app won't start.** No monospace font was found; install one (`sudo apt install fonts-dejavu-core`) or add its path to `FONT_CANDIDATES` in `src/main.rs`.
- **GNOME blanks the screen instead.** Make sure GNOME's own blank isn't racing: `gsettings get org.gnome.desktop.session idle-delay` (0 = never; the daemon owns idle).

---

## Why this exists

GNOME on Wayland dropped animated screensavers (the old subsystem depended on
X11). Reviving one on this particular machine (4K 3840×2160, NVIDIA, XWayland)
meant discovering that every traditional approach is broken here:

- **xscreensaver GL hacks** (`glmatrix`, `glslideshow`, …) **abort** when created
  at 3840×2160; and when created small then resized to fullscreen, the X window
  grows but the **GLX backing buffer stays at the creation size**, so only the
  top-left ~1/9 of the screen renders.
- **xscreensaver 2D Xlib hacks** (`xmatrix`) never receive the fullscreen resize
  event under XWayland → same top-left 1/9 problem.
- **`cmatrix` in a fullscreen terminal** froze itself after one frame on the
  large 4K grid; a curses replacement was janky.

The common thread: anything going through **OpenGL or XWayland** fails on this
box. A native Wayland client that renders pixels on the **CPU** into a `wl_shm`
buffer sidesteps all of it — hence this project.

---

## Contributing

Issues and PRs welcome. The renderer is a single file (`src/main.rs`); the idle
daemon is a single bash script (`dist/wl-screensaver`). Other compositors
(sway, Hyprland, KDE) should mostly work — the renderer is generic Wayland; only
the daemon's Mutter idle query is GNOME-specific, and could be swapped for
`ext-idle-notify` / `swayidle`.

## License

[MIT](LICENSE) © 2026 Asaf Saar
