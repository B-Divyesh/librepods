//! librepods-popup — the iOS/macOS-style "AirPods case opened" card for
//! LibrePods, drawn on a wlr-layer-shell overlay (Hyprland, Sway, river,
//! niri, KDE …).
//!
//! It watches the LibrePods daemon's JSON event stream and shows the card
//! when the case lid opens, when the AirPods connect (a short HUD) or when
//! asked to. Idle it blocks in poll(2) with no timers armed; frames are
//! drawn only while something animates.
//!
//! Derived in part from podctl's `podctl-popup`
//! (https://github.com/Rockykln/podctl, MIT OR Apache-2.0,
//! © 2026 pods contributors) — see the per-file notes and README.md.

mod anim;
mod art;
mod draw;
mod ipc;
mod lock;
mod protocol;
mod render;
mod text;
mod wayland;

use std::io;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anim::{Tween, ease_in_cubic, ease_out_back, ease_out_cubic};
use art::Led;
use ipc::{Cmd, CmdKind, Reply, Watch};
use protocol::{Batt, Device, Event, RingId};
use render::{ButtonKind, Hit, Pose, Press, Renderer, RingView, View};
use wayland::{BTN_LEFT, BTN_RIGHT, Cursor, Input, Wl};

pub static VERBOSE: AtomicBool = AtomicBool::new(false);

#[macro_export]
macro_rules! vlog {
    ($($t:tt)*) => {
        if $crate::VERBOSE.load(std::sync::atomic::Ordering::Relaxed) {
            eprintln!("librepods-popup: {}", format_args!($($t)*));
        }
    };
}

/// How long the card stays up without interaction.
const SHOW_FOR: Duration = Duration::from_secs(7);
/// The "connected" HUD.
const HUD_FOR: Duration = Duration::from_secs(3);
const DEMO_FOR: Duration = Duration::from_secs(8);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const OPEN_TIMEOUT: Duration = Duration::from_secs(2);
const ENTER: Duration = Duration::from_millis(420);
const EXIT: Duration = Duration::from_millis(230);
const RING_DELAY: Duration = Duration::from_millis(140);
const RING_FILL: Duration = Duration::from_millis(650);
const RING_UPDATE: Duration = Duration::from_millis(450);
/// Draw anyway if the compositor sends no frame callback (output off).
const FRAME_FALLBACK: Duration = Duration::from_millis(100);
const LOCK_CACHE: Duration = Duration::from_secs(1);
/// A `lid_open` within this long of the previous one is a repeat (the
/// case re-advertises its lid state) and never shows the card again.
const LID_DEDUPE: Duration = Duration::from_secs(10);

// ─────────────────────────────────── CLI ────────────────────────────────

struct Opts {
    demo: bool,
    model: Option<String>,
    button: Option<ButtonKind>,
    dump: Option<PathBuf>,
    scale: f32,
    backdrop: bool,
    socket: PathBuf,
    font: Option<String>,
    margin: i32,
}

fn usage() {
    println!(
        "librepods-popup {} — AirPods case-open card for LibrePods (Wayland layer-shell)

USAGE
    librepods-popup [--socket PATH]      watch the LibrePods daemon (normal use)
    librepods-popup --demo               show the card with sample data for 8 s
    librepods-popup --dump FILE.png      render one frame to a PNG, no Wayland

OPTIONS
    --socket PATH     daemon socket [default: $XDG_RUNTIME_DIR/librepods.sock]
    --demo            sample card; Connect simulates a connection
    --model M         sample device for --demo/--dump:
                      pro (default), pro1, pro3, 4, 3, 2, max, or 0xID
    --button STATE    sample button: connect, connecting, connected, failed
    --dump FILE       write a PNG of the card (transparent background)
    --scale S         --dump output scale [default: 2]
    --backdrop        --dump onto a sample wallpaper instead of transparency
    --font FAMILY     preferred font family [default: Inter, else SF Pro, Noto Sans]
    --margin PX       extra gap above the bottom edge [default: 0]
    -v, --verbose     log events and decisions to stderr
    -h, --help        this help
    -V, --version     version",
        env!("CARGO_PKG_VERSION")
    );
}

fn parse_args() -> Result<Option<Opts>, String> {
    let mut o = Opts {
        demo: false,
        model: None,
        button: None,
        dump: None,
        scale: 2.0,
        backdrop: false,
        socket: ipc::default_socket(),
        font: None,
        margin: 0,
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut val = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match a.as_str() {
            "-h" | "--help" => {
                usage();
                return Ok(None);
            }
            "-V" | "--version" => {
                println!("librepods-popup {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            "-v" | "--verbose" => VERBOSE.store(true, Ordering::Relaxed),
            "--demo" => o.demo = true,
            "--backdrop" => o.backdrop = true,
            "--model" => o.model = Some(val("--model")?),
            "--socket" => o.socket = PathBuf::from(val("--socket")?),
            "--font" => o.font = Some(val("--font")?),
            "--dump" => o.dump = Some(PathBuf::from(val("--dump")?)),
            "--scale" => {
                o.scale = val("--scale")?
                    .parse::<f32>()
                    .ok()
                    .filter(|s| (0.5..=4.0).contains(s))
                    .ok_or("--scale must be between 0.5 and 4")?
            }
            "--margin" => {
                o.margin = val("--margin")?
                    .parse::<i32>()
                    .ok()
                    .filter(|m| (0..=2000).contains(m))
                    .ok_or("--margin must be 0..2000")?
            }
            "--button" => {
                o.button = Some(match val("--button")?.as_str() {
                    "connect" => ButtonKind::Connect,
                    "connecting" => ButtonKind::Connecting,
                    "connected" => ButtonKind::Connected,
                    "failed" => ButtonKind::Failed,
                    other => return Err(format!("unknown --button state '{other}'")),
                })
            }
            other => return Err(format!("unknown argument '{other}'")),
        }
    }
    Ok(Some(o))
}

fn main() -> ExitCode {
    let opts = match parse_args() {
        Ok(Some(o)) => o,
        Ok(None) => return ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("librepods-popup: {e} (see --help)");
            return ExitCode::from(2);
        }
    };
    let fonts = text::init(opts.font.as_deref());
    vlog!("font: {}", fonts.family);

    if let Some(path) = opts.dump.clone() {
        return dump(&opts, &path);
    }

    // One watcher per session, or a service plus a stray manual run would
    // each pop their own card. Released on exit.
    let _instance = if opts.demo {
        None
    } else {
        match single_instance() {
            Some(f) => Some(f),
            None => {
                eprintln!("librepods-popup: already running in this session");
                return ExitCode::SUCCESS;
            }
        }
    };

    let mut app = App::new(opts);
    let now = Instant::now();
    if app.opts.demo {
        app.dev = protocol::demo_device(app.opts.model.as_deref());
        match app.opts.button {
            Some(ButtonKind::Connected) => app.dev.connected = true,
            Some(ButtonKind::Connecting) => {
                app.link = Link::Connecting {
                    since: now,
                    deadline: now + Duration::from_secs(3600),
                }
            }
            Some(ButtonKind::Failed) => app.link = Link::Failed,
            _ => {}
        }
        app.show(Why::Demo, now);
        if app.card.is_none() {
            return ExitCode::FAILURE;
        }
    } else {
        vlog!("socket: {}", app.opts.socket.display());
        app.watch = Some(Watch::new(app.opts.socket.clone()));
    }
    app.run()
}

// ─────────────────────────────────── app ────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Why {
    Lid,
    Hud,
    Popup,
    Demo,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Link {
    Idle,
    Connecting { since: Instant, deadline: Instant },
    Failed,
}

struct Card {
    /// 0 hidden … 1 at rest.
    vis: Tween,
    leaving: bool,
    timeout: Duration,
    last_touch: Instant,
    hovered: bool,
    hover: Hit,
    pressed: Option<Hit>,
    rings: Vec<(RingId, Tween)>,
    next_spin: Option<Instant>,
    /// Both buffers busy on the last attempt; wait for a release.
    starved: bool,
}

impl Card {
    fn animating(&self, now: Instant) -> bool {
        !self.vis.done(now) || self.rings.iter().any(|(_, t)| !t.done(now))
    }
}

struct App {
    opts: Opts,
    dev: Device,
    link: Link,
    card: Option<Card>,
    wl: Option<Wl>,
    renderer: Renderer,
    watch: Option<Watch>,
    cmds: Vec<Cmd>,
    dirty: bool,
    demo_connect_at: Option<Instant>,
    exit: bool,
    lock_cache: Option<(Instant, bool)>,
    cursor: Cursor,
    /// Previous `lid_open` event, shown or not (dedupe window).
    last_lid_open: Option<Instant>,
    /// This connection has already been announced (or was already up when
    /// we started); further `connected` events for it show nothing.
    announced: bool,
    /// Unit tests: decide show/hide without touching Wayland.
    headless: bool,
}

fn ring_target(b: &Batt) -> f32 {
    b.level.map(|l| l as f32 / 100.0).unwrap_or(0.0)
}

impl App {
    fn new(opts: Opts) -> Self {
        App {
            opts,
            dev: Device::default(),
            link: Link::Idle,
            card: None,
            wl: None,
            renderer: Renderer::default(),
            watch: None,
            cmds: Vec::new(),
            dirty: false,
            demo_connect_at: None,
            exit: false,
            lock_cache: None,
            cursor: Cursor::Default,
            last_lid_open: None,
            announced: false,
            headless: false,
        }
    }

    fn card_up(&self) -> bool {
        self.card.as_ref().is_some_and(|c| !c.leaving)
    }

    fn connecting(&self) -> bool {
        matches!(self.link, Link::Connecting { .. })
    }

    fn locked(&mut self, now: Instant) -> bool {
        if self.headless {
            return false;
        }
        if let Some((t, v)) = self.lock_cache
            && now.duration_since(t) < LOCK_CACHE
        {
            return v;
        }
        let v = lock::session_locked();
        self.lock_cache = Some((now, v));
        v
    }

    // ── showing and hiding ──

    fn show(&mut self, why: Why, now: Instant) {
        let timeout = match why {
            Why::Hud => HUD_FOR,
            Why::Demo => DEMO_FOR,
            Why::Lid | Why::Popup => SHOW_FOR,
        };
        if let Some(c) = self.card.as_mut() {
            if c.leaving {
                c.leaving = false;
                c.vis.retarget(1.0, now, ENTER, ease_out_cubic);
                c.timeout = timeout;
            } else {
                c.timeout = c.timeout.max(timeout);
            }
            c.last_touch = now;
            self.dirty = true;
            return;
        }

        if !self.headless {
            if self.wl.is_none() {
                match Wl::connect() {
                    Ok(w) => self.wl = Some(w),
                    Err(e) => {
                        eprintln!("librepods-popup: wayland: {e}");
                        return;
                    }
                }
            }
            let wl = self.wl.as_mut().expect("wl");
            let (w, h) = render::surface_size();
            if let Err(e) = wl.show(w, h, render::card_rect(), self.opts.margin) {
                eprintln!("librepods-popup: wayland: {e}");
                self.wl = None;
                return;
            }
            vlog!("showing ({why:?}), scale {}", wl.scale());
        }
        if self.dev.connected {
            // Seen on screen for this connection: no HUD for it later.
            self.announced = true;
        }
        let rings = protocol::rings(&self.dev)
            .iter()
            .map(|(id, b)| (*id, Tween::new(0.0, ring_target(b), now + RING_DELAY, RING_FILL, ease_out_cubic)))
            .collect();
        self.card = Some(Card {
            vis: Tween::new(0.0, 1.0, now, ENTER, ease_out_back),
            leaving: false,
            timeout,
            last_touch: now,
            hovered: false,
            hover: Hit::Outside,
            pressed: None,
            rings,
            next_spin: None,
            starved: false,
        });
        self.cursor = Cursor::Default;
        self.dirty = true;
    }

    fn hide(&mut self, now: Instant) {
        if let Some(c) = self.card.as_mut()
            && !c.leaving
        {
            vlog!("hiding");
            c.leaving = true;
            c.pressed = None;
            c.vis.retarget(0.0, now, EXIT, ease_in_cubic);
            self.dirty = true;
        }
    }

    fn destroy_card(&mut self) {
        if let Some(wl) = self.wl.as_mut() {
            wl.hide();
        }
        self.card = None;
        self.renderer.clear();
        if self.link == Link::Failed {
            self.link = Link::Idle;
        }
        if self.opts.demo {
            self.exit = true;
        }
    }

    fn wayland_lost(&mut self, e: &str) {
        eprintln!("librepods-popup: wayland: {e}");
        self.wl = None;
        self.card = None;
        self.renderer.clear();
        if self.opts.demo {
            self.exit = true;
        }
    }

    /// Keep ring animations in step with the device: changed levels glide
    /// from where they are, new rings fill from empty.
    fn sync_rings(&mut self, now: Instant) {
        let want = protocol::rings(&self.dev);
        let Some(card) = self.card.as_mut() else { return };
        let mut next = Vec::with_capacity(want.len());
        for (id, b) in want {
            let target = ring_target(&b);
            match card.rings.iter().find(|(r, _)| *r == id) {
                Some(&(_, mut t)) => {
                    t.retarget(target, now, RING_UPDATE, ease_out_cubic);
                    next.push((id, t));
                }
                None => next.push((id, Tween::new(0.0, target, now, RING_FILL, ease_out_cubic))),
            }
        }
        card.rings = next;
    }

    // ── daemon events ──

    fn on_line(&mut self, line: &str, now: Instant) {
        let Some((ev, obj)) = protocol::parse_line(line) else {
            vlog!("ignoring line: {line}");
            return;
        };
        let was_connected = self.dev.connected;
        self.dev.apply(&obj);
        vlog!("event {ev:?} (connected={}, lid={:?})", self.dev.connected, self.dev.lid_open);

        if !self.dev.connected {
            self.announced = false;
        }
        // Only transition events show the card; `status` lines (including
        // events the user disabled, which the daemon renames to `status`)
        // just update what is on screen.
        match ev {
            Event::LidOpen => {
                let repeat = self.last_lid_open.is_some_and(|t| now.duration_since(t) < LID_DEDUPE);
                self.last_lid_open = Some(now);
                if repeat {
                    // Neither re-show nor keep an open card alive with it.
                    vlog!("repeated lid_open; ignoring");
                } else if self.card_up() {
                    self.touch(now);
                } else if self.locked(now) {
                    vlog!("session locked; not showing");
                } else {
                    self.show(Why::Lid, now);
                }
            }
            Event::LidClose => self.hide(now),
            Event::Popup => self.show(Why::Popup, now),
            Event::Connecting => {
                if !self.dev.connected && !self.connecting() {
                    self.link = Link::Connecting {
                        since: now,
                        deadline: now + CONNECT_TIMEOUT,
                    };
                    self.touch(now);
                }
            }
            Event::ConnectFailed => {
                // Only worth saying on screen; otherwise forget it.
                if self.link != Link::Failed {
                    self.link = if self.card_up() { Link::Failed } else { Link::Idle };
                    self.touch(now);
                }
            }
            Event::Disconnected => {
                if self.connecting() {
                    self.link = Link::Idle;
                }
            }
            _ => {}
        }
        if ev == Event::Connected || (!was_connected && self.dev.connected) {
            self.on_connected(now, ev == Event::Connected);
        }
        self.sync_rings(now);
        self.dirty = true;
    }

    /// The AirPods connected to this machine. A `connected` *event* (not
    /// a status line — the daemon renames events the user disabled)
    /// shows the short HUD when the card isn't already up.
    fn on_connected(&mut self, now: Instant, hud: bool) {
        let ours = matches!(self.link, Link::Connecting { .. } | Link::Failed);
        self.link = Link::Idle;
        let first = !self.announced;
        self.announced = true;
        if !first && !ours {
            // Same connection announced again: nothing new to show.
            vlog!("connection already announced; ignoring");
            return;
        }
        if self.card_up() {
            let c = self.card.as_mut().expect("card");
            let left = (c.last_touch + c.timeout).saturating_duration_since(now);
            if ours || left < HUD_FOR {
                // Show the tick for a moment, then get out of the way.
                c.last_touch = now;
                c.timeout = HUD_FOR;
            }
        } else if hud && !self.locked(now) {
            self.show(Why::Hud, now);
        }
    }

    fn on_reply(&mut self, kind: CmdKind, reply: Reply, now: Instant) {
        vlog!("{kind:?} → {reply:?}");
        if kind == CmdKind::Connect
            && let Reply::Err(_) = reply
            && self.connecting()
            && !self.dev.connected
        {
            self.link = Link::Failed;
            self.touch(now);
            self.dirty = true;
        }
    }

    fn touch(&mut self, now: Instant) {
        if let Some(c) = self.card.as_mut() {
            c.last_touch = now;
        }
    }

    // ── input ──

    fn button_kind(&self) -> ButtonKind {
        if self.dev.connected {
            return ButtonKind::Connected;
        }
        match self.link {
            Link::Idle => ButtonKind::Connect,
            Link::Connecting { .. } => ButtonKind::Connecting,
            Link::Failed => ButtonKind::Failed,
        }
    }

    fn press_of(&self, target: Hit) -> Press {
        let Some(c) = self.card.as_ref() else {
            return Press::Idle;
        };
        if target == Hit::Button && !self.button_kind().clickable() {
            return Press::Idle;
        }
        if c.hover != target {
            Press::Idle
        } else if c.pressed == Some(target) {
            Press::Down
        } else {
            Press::Hover
        }
    }

    fn on_input(&mut self, ev: Input, now: Instant) {
        let Some(card) = self.card.as_mut() else { return };
        if card.leaving {
            return;
        }
        let before = (self.press_of(Hit::Button), self.press_of(Hit::Close));
        let card = self.card.as_mut().expect("card");
        let mut activate = None;
        let hit = |x: f64, y: f64| render::hit(x as f32, y as f32);
        match ev {
            Input::Enter(x, y) => {
                card.hovered = true;
                card.hover = hit(x, y);
                card.last_touch = now;
                // The compositor-side cursor was just reset to the arrow.
                self.cursor = Cursor::Default;
            }
            Input::Motion(x, y) => {
                card.hovered = true;
                card.hover = hit(x, y);
                card.last_touch = now;
            }
            Input::Leave => {
                card.hovered = false;
                card.hover = Hit::Outside;
                card.pressed = None;
                card.last_touch = now;
            }
            Input::Press(BTN_LEFT) => {
                card.pressed = Some(card.hover);
                card.last_touch = now;
            }
            Input::Press(BTN_RIGHT) => {
                // Right-click anywhere dismisses, like a notification.
                activate = Some(Hit::Close);
            }
            Input::Release(BTN_LEFT) => {
                if card.pressed.take() == Some(card.hover) {
                    activate = Some(card.hover);
                }
            }
            Input::TouchDown(x, y) => {
                card.hover = hit(x, y);
                card.pressed = Some(card.hover);
                card.last_touch = now;
            }
            Input::TouchMotion(x, y) => card.hover = hit(x, y),
            Input::TouchUp => {
                let h = std::mem::replace(&mut card.hover, Hit::Outside);
                if card.pressed.take() == Some(h) {
                    activate = Some(h);
                }
                card.last_touch = now;
            }
            Input::TouchCancel => {
                card.pressed = None;
                card.hover = Hit::Outside;
            }
            Input::Press(_) | Input::Release(_) => {}
        }

        let hover = card.hover;
        let want = if hover == Hit::Close || (hover == Hit::Button && self.button_kind().clickable()) {
            Cursor::Pointer
        } else {
            Cursor::Default
        };
        if want != self.cursor
            && let Some(wl) = self.wl.as_ref()
        {
            wl.set_cursor(want);
            self.cursor = want;
        }
        if (self.press_of(Hit::Button), self.press_of(Hit::Close)) != before {
            self.dirty = true;
        }
        if let Some(h) = activate {
            self.activate(h, now);
        }
    }

    fn activate(&mut self, h: Hit, now: Instant) {
        match h {
            Hit::Close => self.hide(now),
            Hit::Button => {
                if self.button_kind().clickable() {
                    self.start_connect(now);
                }
            }
            Hit::Card => {
                self.open_app();
                self.hide(now);
            }
            Hit::Outside => {}
        }
    }

    fn start_connect(&mut self, now: Instant) {
        vlog!("connect requested");
        self.link = Link::Connecting {
            since: now,
            deadline: now + CONNECT_TIMEOUT,
        };
        if self.opts.demo {
            self.demo_connect_at = Some(now + Duration::from_millis(1800));
        } else {
            match ipc::send(&self.opts.socket, CmdKind::Connect, CONNECT_TIMEOUT) {
                Ok(c) => self.cmds.push(c),
                Err(e) => {
                    vlog!("connect: {e}");
                    self.link = Link::Failed;
                }
            }
        }
        self.touch(now);
        self.dirty = true;
    }

    fn open_app(&mut self) {
        if self.opts.demo {
            eprintln!("librepods-popup: (demo) would open the LibrePods window");
            return;
        }
        match ipc::send(&self.opts.socket, CmdKind::Open, OPEN_TIMEOUT) {
            Ok(c) => self.cmds.push(c),
            Err(e) => vlog!("open: {e}"),
        }
    }

    // ── timers ──

    fn tick(&mut self, now: Instant) {
        if let Some(w) = self.watch.as_mut() {
            w.tick(now);
        }
        self.cmds.retain(|c| now < c.deadline);

        if let Some(t) = self.demo_connect_at
            && now >= t
        {
            self.demo_connect_at = None;
            self.dev.connected = true;
            self.on_connected(now, false);
            self.dirty = true;
        }
        if let Link::Connecting { deadline, .. } = self.link
            && now >= deadline
        {
            vlog!("connect timed out");
            self.link = if self.card_up() { Link::Failed } else { Link::Idle };
            self.touch(now);
            self.dirty = true;
        }

        let connecting = self.connecting();
        let Some(c) = self.card.as_ref() else { return };
        if c.leaving {
            if c.vis.done(now) {
                self.destroy_card();
            }
        } else if !c.hovered && c.pressed.is_none() && !connecting && now >= c.last_touch + c.timeout {
            self.hide(now);
        }
    }

    fn next_wake(&self, now: Instant) -> Option<Instant> {
        let mut t: Option<Instant> = None;
        let mut at = |x: Option<Instant>| {
            if let Some(x) = x {
                t = Some(t.map_or(x, |t| t.min(x)));
            }
        };
        at(self.watch.as_ref().and_then(Watch::deadline));
        for c in &self.cmds {
            at(Some(c.deadline));
        }
        at(self.demo_connect_at);
        if let Link::Connecting { deadline, .. } = self.link {
            at(Some(deadline));
        }
        if let Some(c) = &self.card {
            if !c.leaving && !c.hovered && c.pressed.is_none() && !self.connecting() {
                at(Some(c.last_touch + c.timeout));
            }
            if c.leaving && c.vis.done(now) {
                at(Some(now));
            }
            at(c.next_spin);
            if let Some(wl) = &self.wl {
                if let Some(since) = wl.frame_pending() {
                    at(Some(since + FRAME_FALLBACK));
                } else if c.starved {
                    at(Some(now + FRAME_FALLBACK));
                } else if self.dirty || c.animating(now) {
                    at(Some(now));
                }
            }
        }
        t
    }

    // ── drawing ──

    fn view(&self, now: Instant) -> View {
        let card = self.card.as_ref().expect("card");
        let rings = protocol::rings(&self.dev)
            .into_iter()
            .map(|(id, batt)| RingView {
                id,
                batt,
                fill: card
                    .rings
                    .iter()
                    .find(|(r, _)| *r == id)
                    .map(|(_, t)| t.value(now))
                    .unwrap_or(0.0),
            })
            .collect();
        // Apple's case light: amber while charging or below one full
        // charge, green when full; it lights while the lid is open.
        let led = match self.dev.case {
            Some(Batt {
                level: Some(100), ..
            }) => Led::Full,
            Some(Batt { charging: true, .. }) => Led::Charging,
            Some(Batt { level: Some(l), .. }) if self.dev.lid_open == Some(true) => {
                if l >= 30 {
                    Led::Full
                } else {
                    Led::Charging
                }
            }
            _ => Led::Off,
        };
        let spin = match self.link {
            Link::Connecting { since, .. } => now.duration_since(since).as_secs_f32() * render::SPINNER_HZ / 8.0,
            _ => 0.0,
        };
        View {
            title: self.dev.title(),
            product: self.dev.product(),
            led,
            rings,
            button: self.button_kind(),
            button_press: self.press_of(Hit::Button),
            close_press: self.press_of(Hit::Close),
            spin,
        }
    }

    fn redraw(&mut self, now: Instant) {
        if self.card.is_none() {
            return;
        }
        let Some(wl) = self.wl.as_mut() else { return };
        if wl.st.frame_done {
            wl.clear_frame_pending();
        }
        if wl.st.scale_changed {
            wl.st.scale_changed = false;
            self.dirty = true;
        }
        if let Some(since) = wl.frame_pending() {
            if now.duration_since(since) < FRAME_FALLBACK {
                return;
            }
            wl.clear_frame_pending();
        }
        let card = self.card.as_ref().expect("card");
        let animating = card.animating(now);
        let spin_due = card.next_spin.is_some_and(|t| now >= t);
        if !(self.dirty || animating || spin_due) {
            return;
        }

        let view = self.view(now);
        let pose = Pose::at(card.vis.value(now));
        let wl = self.wl.as_mut().expect("wl");
        let renderer = &mut self.renderer;
        let res = wl.draw(animating, |buf, w, h, fmt, scale| {
            renderer.render(buf, w, h, fmt, scale, &view, pose);
        });
        match res {
            Ok(true) => {
                self.dirty = false;
                let next_spin = match self.link {
                    Link::Connecting { since, .. } if view.button == ButtonKind::Connecting => {
                        let spokes = now.duration_since(since).as_secs_f32() * render::SPINNER_HZ;
                        Some(since + Duration::from_secs_f32((spokes.floor() + 1.0) / render::SPINNER_HZ))
                    }
                    _ => None,
                };
                let c = self.card.as_mut().expect("card");
                c.next_spin = next_spin;
                c.starved = false;
            }
            Ok(false) => self.card.as_mut().expect("card").starved = true,
            Err(e) => self.wayland_lost(&e),
        }
    }

    fn process_wayland(&mut self, now: Instant) {
        let Some(wl) = self.wl.as_mut() else { return };
        let inputs: Vec<Input> = wl.st.input.drain(..).collect();
        if wl.st.closed {
            wl.st.closed = false;
            vlog!("compositor closed the surface");
            self.destroy_card();
            return;
        }
        if let Some(c) = self.card.as_mut()
            && c.starved
        {
            // A release may have arrived; try again.
            c.starved = false;
            self.dirty = true;
        }
        for i in inputs {
            self.on_input(i, now);
        }
    }

    // ── main loop ──

    fn run(&mut self) -> ExitCode {
        loop {
            let now = Instant::now();
            self.process_wayland(now);
            self.tick(now);
            self.redraw(now);
            if self.exit {
                return ExitCode::SUCCESS;
            }

            let guard = match self.wl.as_mut().map(Wl::prepare) {
                None => None,
                Some(Ok(Some(g))) => Some(g),
                Some(Ok(None)) => continue,
                Some(Err(e)) => {
                    self.wayland_lost(&e);
                    continue;
                }
            };

            let timeout_ms = match self.next_wake(Instant::now()) {
                None => -1,
                Some(t) => {
                    let d = t.saturating_duration_since(Instant::now());
                    d.as_micros().div_ceil(1000).min(i32::MAX as u128) as i32
                }
            };

            let pfd = |fd| libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            let mut fds = Vec::with_capacity(2 + self.cmds.len());
            let wl_i = guard.as_ref().map(|g| {
                fds.push(pfd(g.connection_fd().as_raw_fd()));
                fds.len() - 1
            });
            let watch_i = self.watch.as_ref().and_then(Watch::fd).map(|fd| {
                fds.push(pfd(fd));
                fds.len() - 1
            });
            let cmd0 = fds.len();
            for c in &self.cmds {
                fds.push(pfd(c.fd()));
            }

            // SAFETY: `fds` is a live, correctly sized array of pollfd.
            let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout_ms) };
            if n < 0 {
                let e = io::Error::last_os_error();
                if e.kind() != io::ErrorKind::Interrupted {
                    eprintln!("librepods-popup: poll: {e}");
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
            let now = Instant::now();
            let ready = |i: Option<usize>| i.is_some_and(|i| n > 0 && fds[i].revents != 0);

            if let Some(g) = guard {
                let readable = ready(wl_i);
                if let Err(e) = self.wl.as_mut().expect("wl").after_poll(g, readable) {
                    self.wayland_lost(&e);
                }
            }
            if ready(watch_i) {
                let lines = self.watch.as_mut().map(|w| w.read(now)).unwrap_or_default();
                for l in lines {
                    self.on_line(&l, now);
                }
            }
            let done: Vec<usize> = (0..self.cmds.len()).filter(|&k| ready(Some(cmd0 + k))).collect();
            for k in done.into_iter().rev() {
                if let Some(reply) = self.cmds[k].read() {
                    let c = self.cmds.remove(k);
                    self.on_reply(c.kind, reply, now);
                }
            }
        }
    }
}

// ────────────────────────────────── misc ────────────────────────────────

fn single_instance() -> Option<std::fs::File> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(dir.join("librepods-popup.lock"))
        .ok()?;
    // SAFETY: flock on an fd we own.
    let ok = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    ok.then_some(file)
}

/// `--dump`: one still frame, rings full, to a PNG.
fn dump(opts: &Opts, path: &std::path::Path) -> ExitCode {
    let dev = protocol::demo_device(opts.model.as_deref());
    let rings = protocol::rings(&dev)
        .into_iter()
        .map(|(id, batt)| RingView {
            id,
            batt,
            fill: ring_target(&batt),
        })
        .collect();
    let view = View {
        title: dev.title(),
        product: dev.product(),
        led: if dev.case.is_some_and(|c| c.charging) {
            Led::Charging
        } else {
            Led::Off
        },
        rings,
        button: opts.button.unwrap_or(if dev.connected {
            ButtonKind::Connected
        } else {
            ButtonKind::Connect
        }),
        button_press: Press::Idle,
        close_press: Press::Idle,
        spin: 0.3,
    };
    let (w, h) = render::surface_size();
    let (pw, ph) = ((w as f32 * opts.scale).round() as u32, (h as f32 * opts.scale).round() as u32);
    let mut buf = vec![0u8; (pw * ph * 4) as usize];
    Renderer::default().render(
        &mut buf,
        pw,
        ph,
        render::Format::Rgba,
        pw as f32 / w as f32,
        &view,
        Pose::REST,
    );
    let Some(card) = tiny_skia::IntSize::from_wh(pw, ph).and_then(|s| tiny_skia::Pixmap::from_vec(buf, s)) else {
        eprintln!("librepods-popup: bad dump size");
        return ExitCode::FAILURE;
    };
    let out = if opts.backdrop {
        let mut bg = tiny_skia::Pixmap::new(pw, ph).expect("backdrop");
        let g = draw::lin(
            (0.0, 0.0),
            (pw as f32, ph as f32),
            &[(0.0, draw::rgb(64, 84, 128)), (1.0, draw::rgb(140, 96, 140))],
        );
        bg.fill_rect(
            tiny_skia::Rect::from_xywh(0.0, 0.0, pw as f32, ph as f32).expect("rect"),
            &draw::paint(g, draw::rgb(80, 90, 120)),
            tiny_skia::Transform::identity(),
            None,
        );
        bg.draw_pixmap(
            0,
            0,
            card.as_ref(),
            &tiny_skia::PixmapPaint::default(),
            tiny_skia::Transform::identity(),
            None,
        );
        bg
    } else {
        card
    };
    match out.save_png(path) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("librepods-popup: {}: {e}", path.display());
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: Duration = Duration::from_secs(1);

    fn app() -> App {
        let mut a = App::new(Opts {
            demo: false,
            model: None,
            button: None,
            dump: None,
            scale: 1.0,
            backdrop: false,
            socket: PathBuf::from("/nonexistent"),
            font: None,
            margin: 0,
        });
        a.headless = true;
        a
    }

    fn line(ev: &str, connected: bool, lid: bool) -> String {
        format!(
            r#"{{"event":"{ev}","connected":{connected},"name":"AirPods Pro","model_id":8228,"left":{{"level":80,"charging":false}},"right":{{"level":79,"charging":false}},"case":{{"level":50,"charging":false}},"lid_open":{lid}}}"#
        )
    }

    /// Let the card time out and finish its exit animation.
    fn settle(a: &mut App, now: Instant) -> Instant {
        let t = now + SHOW_FOR + S;
        a.tick(t);
        let t = t + EXIT + S;
        a.tick(t);
        t
    }

    #[test]
    fn status_lines_never_show() {
        let mut a = app();
        let t = Instant::now();
        a.on_line(&line("status", true, true), t);
        a.on_line(&line("status", false, true), t + S);
        a.on_line(&line("status", true, true), t + 2 * S);
        assert!(a.card.is_none());
    }

    #[test]
    fn already_connected_at_startup_is_not_announced() {
        let mut a = app();
        let t = Instant::now();
        a.on_line(&line("status", true, false), t);
        a.on_line(&line("connected", true, false), t + S);
        assert!(a.card.is_none(), "same connection as the first status line");
    }

    #[test]
    fn lid_open_shows_once_per_window() {
        let mut a = app();
        let t = Instant::now();
        a.on_line(&line("lid_open", false, true), t);
        assert!(a.card_up());
        let touched = a.card.as_ref().unwrap().last_touch;
        a.on_line(&line("lid_open", false, true), t + 2 * S);
        assert_eq!(a.card.as_ref().unwrap().last_touch, touched, "a repeat must not keep the card alive");
        let t2 = settle(&mut a, t + 2 * S);
        assert!(a.card.is_none());
        // Repeats keep arriving: still within 10 s of the previous one.
        a.on_line(&line("lid_open", false, true), t2);
        assert!(a.card.is_none(), "repeated lid_open must not re-show");
        // A genuinely new opening after a quiet period shows again.
        a.on_line(&line("lid_close", false, false), t2 + S);
        a.on_line(&line("lid_open", false, true), t2 + LID_DEDUPE + 2 * S);
        assert!(a.card_up());
    }

    #[test]
    fn connected_hud_once_per_connection() {
        let mut a = app();
        let t = Instant::now();
        a.on_line(&line("status", false, false), t);
        a.on_line(&line("connected", true, false), t + S);
        assert!(a.card_up(), "first connect shows the HUD");
        assert_eq!(a.card.as_ref().unwrap().timeout, HUD_FOR);
        let t2 = settle(&mut a, t + S);
        assert!(a.card.is_none());
        a.on_line(&line("connected", true, false), t2);
        assert!(a.card.is_none(), "duplicate connected must not re-show");
        // A lid_open shown while connected also counts as announced.
        a.on_line(&line("disconnected", false, false), t2 + S);
        a.on_line(&line("lid_open", true, true), t2 + 2 * S);
        assert!(a.card_up());
        let t3 = settle(&mut a, t2 + 2 * S);
        a.on_line(&line("connected", true, true), t3);
        assert!(a.card.is_none(), "card was already shown for this connection");
        // After a real disconnect, the next connection is announced.
        a.on_line(&line("disconnected", false, false), t3 + S);
        a.on_line(&line("connected", true, false), t3 + 2 * S);
        assert!(a.card_up());
    }

    #[test]
    fn popup_always_shows() {
        let mut a = app();
        let t = Instant::now();
        a.on_line(&line("status", true, false), t);
        a.on_line(&line("popup", true, false), t + S);
        assert!(a.card_up());
    }

    #[test]
    fn connect_flow_states() {
        let mut a = app();
        let t = Instant::now();
        a.on_line(&line("lid_open", false, true), t);
        assert_eq!(a.button_kind(), ButtonKind::Connect);
        a.on_line(&line("connecting", false, true), t + S);
        assert_eq!(a.button_kind(), ButtonKind::Connecting);
        a.on_line(&line("connect_failed", false, true), t + 2 * S);
        assert_eq!(a.button_kind(), ButtonKind::Failed);
        a.on_line(&line("connecting", false, true), t + 3 * S);
        a.tick(t + 3 * S + CONNECT_TIMEOUT + S);
        assert_eq!(a.button_kind(), ButtonKind::Failed, "10 s without a connection");
        a.on_line(&line("connected", true, true), t + 15 * S);
        assert_eq!(a.button_kind(), ButtonKind::Connected);
        assert_eq!(a.card.as_ref().unwrap().timeout, HUD_FOR, "hides soon after connecting");
    }
}
