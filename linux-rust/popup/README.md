# librepods-popup

The iPhone/Mac "AirPods case opened" card for LibrePods on Wayland.

Open the case (or connect, or press your "show AirPods" keybind) and an
iOS-style card slides up from the bottom of the screen. It uses the desktop's
Ember theme, the same palette as Waybar, mako and the LibrePods window. It shows
the device name, an illustration of your model (AirPods Pro, AirPods 3/4,
AirPods 1/2 or AirPods Max), battery rings for the left and right buds (merged
into one "AirPods" ring when they're within 5 %) and the case, and a
**Connect** button.

| | |
|---|---|
| card | `#1a141b` at 92 % (the compositor blurs what's behind it), 1 px `rgba(232,155,94,.35)` border, 22 px radius |
| text | `#f0e4d4` / `#bfa998` / `#7d6a66` (primary / secondary / tertiary), Inter |
| Connect | amber `#e89b5e` pill, `#1a141b` text; "Connected" `#8cb3d9`; "Couldn't Connect" `#e5534b` |
| rings | green `#a8c686`, gold `#efcf9f` at ≤ 20 %, red `#e5534b` at ≤ 10 % (Waybar's thresholds), track `rgba(240,228,212,.12)`; green charging bolt from JetBrainsMono Nerd Font Propo (U+F140B) |

It's a small standalone binary (one thread, ~5 MB RSS) that draws on a
`wlr-layer-shell` overlay surface with raw `wl_shm` buffers: no GTK, no Qt,
no GPU. It runs on Hyprland, Sway, river, niri, Wayfire and KDE, and on any
other compositor that offers layer-shell.

## Build and run

```sh
cargo build --release            # → target/release/librepods-popup
target/release/librepods-popup --demo          # sample card for 8 s
target/release/librepods-popup                 # watch the LibrePods daemon
```

Run it once per graphical session. One option is Hyprland's autostart
(Lua config: add it to the `startup` list in `conf/autostart.lua`):

```lua
"~/src/librepods/linux-rust/popup/target/release/librepods-popup",
```

Another is a systemd user service next to `librepods.service`. The user
manager needs `WAYLAND_DISPLAY`; the autostart already imports it with
`systemctl --user import-environment`.

```ini
# ~/.config/systemd/user/librepods-popup.service
[Unit]
Description=LibrePods case-open card
PartOf=graphical-session.target
After=graphical-session.target

[Service]
ExecStart=%h/src/librepods/linux-rust/popup/target/release/librepods-popup
Restart=on-failure

[Install]
WantedBy=graphical-session.target
```

A lock file (`$XDG_RUNTIME_DIR/librepods-popup.lock`) allows only one watcher
per session. If the daemon isn't running yet, or restarts, the popup retries
its socket silently every 2 s. It connects to Wayland only when it first has
something to show.

### Hyprland layer rule

The surface namespace is `librepods-popup`. The card is translucent, so give
it compositor blur (this is the rule in `conf/rules.lua`):

```lua
hl.layer_rule({ name = "librepods-card", match = { namespace = "^(librepods-popup)$" },
                blur = true, ignore_alpha = 0.3, animation = "slide bottom" })
```

```ini
# hyprlang equivalent
layerrule = blur, librepods-popup
layerrule = ignorealpha 0.3, librepods-popup
```

The card's soft drop shadow never goes above alpha 0.27, so with
`ignore_alpha 0.3` the blur stays inside the card (alpha 0.92) and doesn't
frost a halo around it. The card also runs its own short slide-and-fade,
which works with or without a compositor layer animation.

## When it shows

Only these transition *events* show the card. A plain `status` line never
does, and that includes events the user turned off, which the daemon sends
as `status`.

| daemon event       | card                                                                                     |
|--------------------|------------------------------------------------------------------------------------------|
| `lid_open`         | shows for 7 s (not while the session is locked). A `lid_open` within 10 s of the previous one is a repeat: it neither re-shows nor extends the card. |
| `connected`        | if the card isn't up: a 3 s "Connected" HUD (not while locked); if it's up: ✓, then it hides after 3 s. Once per connection: repeats, a connection already up when the popup starts, and a connection the card was already shown for are all ignored until the next disconnect. |
| `popup`            | shows for 7 s, even when locked (explicit request: keybind / CLI)                        |
| `lid_close`        | hides                                                                                    |
| `connecting`       | spinner on the button (if the card is up)                                                |
| `connect_failed`   | "Couldn't Connect · Try Again"                                                           |
| anything else      | contents update live (battery often arrives just after `lid_open`)                       |

The hide timer pauses while the pointer is over the card and restarts when it
leaves. To dismiss the card early, click the ✕ or right-click anywhere on it.
The card has no keyboard focus (`keyboard_interactivity = none`: it never
steals focus), so Escape isn't available, and layer-shell can't report clicks
outside a surface.

"Locked" means a screen locker process (`hyprlock`, `swaylock`, `gtklock`,
`waylock`, …) is running, or logind's `LockedHint` is set.

### Clicks

* **Connect** (blue pill, shown when `connected` is false): sends `connect`
  and shows a spinner until a `connected` event arrives. On an
  `{"ok":false}` reply, a `connect_failed` event, or no connection within
  10 s, it shows "Couldn't Connect · Try Again", and clicking retries.
  When connected, the button reads "Connected" (disabled).
* **✕** or **right-click**: dismiss.
* **Anywhere else on the card**: sends `open` (raise the LibrePods window)
  and dismisses.

Touchscreen taps work too, and the pointer turns into a hand over the
buttons (cursor-shape-v1).

## Protocol

Socket: `$XDG_RUNTIME_DIR/librepods.sock` (override with `--socket PATH`).

**Watch stream.** The popup connects and writes `watch popup\n`. The daemon
replies with one JSON object per line: first the current state, then one per
change. The daemon applies the user's settings: if the case popup or the
connect card is disabled, those events arrive renamed to `status`.

```json
{"event":"lid_open","connected":false,"nearby":true,"name":"AirPods Pro",
 "model":"AirPods Pro 2 (USB-C)","model_id":8228,"mac":"AC:DE:48:00:11:22",
 "left":{"level":80,"charging":false},"right":{"level":78,"charging":false},
 "case":{"level":55,"charging":true},"headphone":null,
 "in_ear":{"left":false,"right":false},"in_case":{"left":true,"right":true},
 "lid_open":true,"listening_mode":"anc","conversational_awareness":true,"auto_connect":true}
```

* `event`: `status | lid_open | lid_close | connected | disconnected | connecting | connect_failed | mode | low_battery | popup`.
* Only the keys present in a line are applied, so partial objects are fine.
  A key sent as `null` means unknown and clears the old value.
* Battery levels are 0–100. Anything else (`-1`, `255`) counts as unknown.
  An unknown ring shows a dash.
* `model_id` picks the illustration: `0x200E/0x2014/0x2024/0x2027` → Pro,
  `0x2013/0x2019/0x201B` → AirPods 3/4, `0x2002/0x200F` → AirPods 1/2,
  `0x200A/0x201F` → Max (one "Battery" ring from `headphone`). Unknown ids
  fall back on the `model`/`name` strings, then on generic buds.
* The title is `name`, falling back to `model`.

**Commands.** Each command uses its own short-lived connection and gets one
reply line:

| send        | reply                                   |
|-------------|-----------------------------------------|
| `connect\n` | `{"ok":true}` or `{"ok":false,"error":"…"}` |
| `open\n`    | (read and ignored)                      |

## Options

```
--socket PATH     daemon socket [default: $XDG_RUNTIME_DIR/librepods.sock]
--demo            sample card for 8 s; Connect simulates a connection
--model M         sample device for --demo/--dump: pro, pro1, pro3, 4, 3, 2, max, 0xID
--button STATE    sample button: connect, connecting, connected, failed
--dump FILE       render one frame to a PNG (no Wayland needed)
--scale S         --dump scale [default: 2]
--backdrop        --dump onto a sample wallpaper instead of transparency
--font FAMILY     preferred font [default: Inter, else SF Pro, Noto Sans, embedded Noto]
--margin PX       extra gap above the bottom edge [default: 0; the card already sits 24 px up]
-v, --verbose     log events and decisions to stderr
```

## How it draws, and what it costs

* **Idle:** the process blocks in `poll(2)` on the daemon socket with no
  timers armed. It uses 0 % CPU and has no Wayland surface. Frames are drawn
  only while something moves: the slide-in (~0.4 s), the ring fill (0.65 s),
  the slide-out (0.23 s), paced by the compositor's frame callbacks. The
  "Connecting…" spinner ticks at 12 fps like UIKit's, not 60.
* **Layers:** the shadow, card body, text, illustration and static ring
  parts are composited once per content change and cached. A resting frame
  is one copy plus the moving ring arcs; a sliding/fading frame is one
  scaled copy. That's 1–2 ms at 2× on an i7-8550U. The first frame,
  including the blurred drop shadow, takes about 35 ms. A whole show
  (slide-in, ring fill, 8 s up, slide-out) costs about 0.25 s of CPU.
  Idle, the process sits at about 5 MB RSS. The frame buffers (about 20 MB
  at 2×) exist only while the card is up.
* **HiDPI:** rendering uses `wp_fractional_scale_v1` + `wp_viewporter` when
  available (exact 1.25×, 1.5×, … buffers). Otherwise it falls back to
  `preferred_buffer_scale` or the output scale, as an integer.
* **Text:** Inter (falling back to SF Pro or Noto), found via fontconfig,
  memory-mapped and shaped with rustybuzz (kerning, tabular figures). Glyphs
  are rasterised with exact area coverage at sub-pixel positions. The Noto
  Sans Regular embedded in the binary is the last resort. Icons come from
  JetBrainsMono Nerd Font Propo, with vector fallbacks.
* **Illustrations:** vector shapes with gradients, drawn by the code in
  `src/art.rs`. No Apple artwork is included.

## Layout

```
src/main.rs      CLI, state machine (show/hide/connect), poll loop
src/protocol.rs  JSON event model, product ids, ring selection
src/ipc.rs       watch stream + connect/open commands
src/wayland.rs   layer-shell surface, shm buffers, input, scaling
src/render.rs    card layout, rings, button, shadow, compositing
src/art.rs       product illustrations
src/text.rs      font discovery, shaping, glyph rasterisation
src/draw.rs      tiny-skia helpers, continuous-corner rect, blur
src/anim.rs      easing + tween
src/lock.rs      session-locked check
```

## Credits and licence

`librepods-popup` is licensed under **MIT OR Apache-2.0** (`LICENSE-MIT`,
`LICENSE-APACHE`). That matches the code it is derived from, and it can be
combined with LibrePods (GPL-3.0).

Parts are ported from **podctl**'s `podctl-popup`
(<https://github.com/Rockykln/podctl>, © 2026 pods contributors, MIT OR
Apache-2.0):

* The Wayland layer-shell backend: the memfd double-buffered `wl_shm` pool
  on an overlay surface (`src/wayland.rs`, from `backend/wl_layer.rs`).
* The tiny-skia card rendering: gradient body with a hairline, battery
  donuts with a colour ramp, fitted titles (`src/render.rs`, `src/draw.rs`,
  `src/text.rs`, from `render.rs`).
* The earbud drawn as layered, individually shaded shapes with contact
  shadows (`src/art.rs`, from `assets.rs`).
* Easing (`src/anim.rs`, from `anim.rs`).

Each ported file starts with a comment saying what it came from. The X11
and notification backends, the config file and the podctl daemon protocol
were not ported.

The embedded fallback font is **Noto Sans Regular** © Google Inc., under the
Apache License 2.0 (`assets/fonts/LICENSE-NotoSans.txt`), unmodified, as
bundled by podctl.
