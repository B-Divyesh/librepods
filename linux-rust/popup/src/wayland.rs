//! wlr-layer-shell overlay surfaces on raw wl_shm buffers.
//!
//! Adapted from podctl's `src/bin/popup/backend/wl_layer.rs`
//! (https://github.com/Rockykln/podctl, MIT OR Apache-2.0,
//! © 2026 pods contributors): memfd-backed double-buffered wl_shm pool,
//! overlay layer anchored to the bottom edge, no keyboard interactivity.
//!
//! Additions here: one connection driven from the caller's poll loop (no
//! blocking dispatch) serving two independent surfaces, the case-open
//! card and the bar dropdown menu; pointer, touch and keyboard input
//! routed per surface; input regions; frame callbacks for pacing;
//! fractional scaling via wp_fractional_scale_v1 + wp_viewporter (integer
//! `preferred_buffer_scale` / output scale otherwise); cursor shapes; and
//! rendering straight into the shm buffer when the compositor takes
//! ABGR8888.
//!
//! The menu covers the whole output (so any click outside the panel can
//! close it) without paying for a full-screen buffer: the layer surface
//! holds a single transparent pixel stretched by wp_viewporter, and the
//! panel is a small desynchronised subsurface on top of it.

use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::time::Instant;

use wayland_client::backend::{ObjectId, ReadEventsGuard, WaylandError};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{
    wl_buffer::{self, WlBuffer},
    wl_callback::{self, WlCallback},
    wl_compositor::WlCompositor,
    wl_keyboard::{self, WlKeyboard},
    wl_output::{self, WlOutput},
    wl_pointer::{self, WlPointer},
    wl_region::WlRegion,
    wl_registry::WlRegistry,
    wl_seat::{self, WlSeat},
    wl_shm::{self, WlShm},
    wl_shm_pool::WlShmPool,
    wl_subcompositor::WlSubcompositor,
    wl_subsurface::WlSubsurface,
    wl_surface::{self, WlSurface},
    wl_touch::{self, WlTouch},
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::wp::cursor_shape::v1::client::{
    wp_cursor_shape_device_v1::{self, WpCursorShapeDeviceV1},
    wp_cursor_shape_manager_v1::WpCursorShapeManagerV1,
};
use wayland_protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
    wp_fractional_scale_v1::{self, WpFractionalScaleV1},
};
use wayland_protocols::wp::viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{self, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1},
};

use crate::render::Format;

pub const CARD_NAMESPACE: &str = "librepods-popup";
pub const MENU_NAMESPACE: &str = "librepods-menu";

/// Linux input event codes.
pub const BTN_LEFT: u32 = 0x110;
pub const BTN_RIGHT: u32 = 0x111;
pub const KEY_ESC: u32 = 1;

/// Which window an input event or state belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    Card,
    Menu,
}

/// Role of one of our wl_surfaces (its user data).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tag {
    Card,
    MenuBg,
    MenuPanel,
}

impl Tag {
    fn target(self) -> Target {
        match self {
            Tag::Card => Target::Card,
            Tag::MenuBg | Tag::MenuPanel => Target::Menu,
        }
    }
}

/// Coordinates are logical px relative to the target's layer surface (for
/// the menu: the whole output, even over the panel subsurface).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Input {
    Enter(Target, f64, f64),
    Motion(Target, f64, f64),
    Leave(Target),
    Press(Target, u32),
    Release(Target, u32),
    TouchDown(Target, f64, f64),
    TouchMotion(Target, f64, f64),
    TouchUp(Target),
    TouchCancel,
    /// A key went down (evdev code) while the menu held the keyboard.
    Key(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cursor {
    Default,
    Pointer,
}

/// Per-surface events the app reads after each dispatch.
#[derive(Default)]
pub struct SurfEv {
    configured: bool,
    size: (u32, u32),
    pub closed: bool,
    frac: Option<u32>,
    pref_int: Option<i32>,
    entered: Vec<ObjectId>,
    pub scale_changed: bool,
    pub frame_done: bool,
}

/// Event sink for the dispatch handlers.
#[derive(Default)]
pub struct St {
    formats: Vec<wl_shm::Format>,
    seat: Option<WlSeat>,
    pointer: Option<WlPointer>,
    touch: Option<WlTouch>,
    keyboard: Option<WlKeyboard>,
    cursor_mgr: Option<WpCursorShapeManagerV1>,
    cursor_dev: Option<WpCursorShapeDeviceV1>,
    pointer_serial: Option<u32>,
    /// Pointer focus: target and the offset of the entered surface.
    ptr: Option<(Target, f64, f64)>,
    touch_focus: Option<(i32, Target, f64, f64)>,
    /// Menu panel subsurface position within the menu surface.
    panel_offset: (f64, f64),
    /// (output, scale, x in the global space)
    outputs: Vec<(WlOutput, i32, i32)>,
    pub input: Vec<Input>,
    pub card: SurfEv,
    pub menu: SurfEv,
    released: Vec<u32>,
}

impl St {
    pub fn ev(&mut self, t: Target) -> &mut SurfEv {
        match t {
            Target::Card => &mut self.card,
            Target::Menu => &mut self.menu,
        }
    }

    fn ev_ref(&self, t: Target) -> &SurfEv {
        match t {
            Target::Card => &self.card,
            Target::Menu => &self.menu,
        }
    }

    fn offset(&self, tag: Tag) -> (f64, f64) {
        if tag == Tag::MenuPanel { self.panel_offset } else { (0.0, 0.0) }
    }
}

struct Pool {
    pool: WlShmPool,
    _fd: OwnedFd,
    map: *mut u8,
    len: usize,
    w: u32,
    h: u32,
    bufs: [WlBuffer; 2],
    busy: [bool; 2],
    id: u32,
    format: Format,
}

impl Pool {
    fn destroy(self) {
        for b in &self.bufs {
            b.destroy();
        }
        self.pool.destroy();
        // SAFETY: `map`/`len` came from a successful mmap and are unmapped
        // exactly once, here.
        unsafe {
            libc::munmap(self.map as *mut libc::c_void, self.len);
        }
    }
}

/// A surface we draw frames into.
struct Canvas {
    surface: WlSurface,
    tag: Tag,
    viewport: Option<WpViewport>,
    pool: Option<Pool>,
    /// Logical size.
    w: u32,
    h: u32,
    frame_pending: Option<Instant>,
    applied_buffer_scale: i32,
    dest: Option<(u32, u32)>,
}

impl Canvas {
    fn destroy(self) {
        if let Some(p) = self.pool {
            p.destroy();
        }
        if let Some(v) = self.viewport {
            v.destroy();
        }
        self.surface.destroy();
    }
}

struct CardSurf {
    layer: ZwlrLayerSurfaceV1,
    frac: Option<WpFractionalScaleV1>,
    canvas: Canvas,
}

struct MenuSurf {
    layer: ZwlrLayerSurfaceV1,
    frac: Option<WpFractionalScaleV1>,
    bg: WlSurface,
    bg_viewport: Option<WpViewport>,
    bg_pool: Option<Pool>,
    sub: Option<WlSubsurface>,
    panel: Option<Canvas>,
}

pub struct Wl {
    conn: Connection,
    queue: EventQueue<St>,
    qh: QueueHandle<St>,
    pub st: St,
    compositor: WlCompositor,
    subcompositor: Option<WlSubcompositor>,
    shm: WlShm,
    layer_shell: ZwlrLayerShellV1,
    viewporter: Option<WpViewporter>,
    frac_mgr: Option<WpFractionalScaleManagerV1>,
    card: Option<CardSurf>,
    menu: Option<MenuSurf>,
    next_pool: u32,
}

fn err<E: std::fmt::Display>(what: &str) -> impl FnOnce(E) -> String + '_ {
    move |e| format!("{what}: {e}")
}

impl Wl {
    pub fn connect() -> Result<Wl, String> {
        let conn = Connection::connect_to_env().map_err(err("connect to Wayland display"))?;
        let (globals, mut queue) = registry_queue_init::<St>(&conn).map_err(err("registry"))?;
        let qh = queue.handle();

        let compositor: WlCompositor = globals.bind(&qh, 4..=6, ()).map_err(err("wl_compositor"))?;
        let subcompositor: Option<WlSubcompositor> = globals.bind(&qh, 1..=1, ()).ok();
        let shm: WlShm = globals.bind(&qh, 1..=1, ()).map_err(err("wl_shm"))?;
        let layer_shell: ZwlrLayerShellV1 = globals
            .bind(&qh, 1..=4, ())
            .map_err(|_| "compositor has no wlr-layer-shell".to_string())?;
        let viewporter: Option<WpViewporter> = globals.bind(&qh, 1..=1, ()).ok();
        let frac_mgr: Option<WpFractionalScaleManagerV1> = globals.bind(&qh, 1..=1, ()).ok();

        let mut st = St {
            cursor_mgr: globals.bind(&qh, 1..=1, ()).ok(),
            ..Default::default()
        };
        st.seat = globals.bind::<WlSeat, _, _>(&qh, 1..=7, ()).ok();
        for g in globals.contents().clone_list() {
            if g.interface == WlOutput::interface().name {
                let o: WlOutput = globals.registry().bind(g.name, g.version.clamp(1, 4), &qh, ());
                st.outputs.push((o, 1, 0));
            }
        }
        // Collect shm formats, seat capabilities and output scales.
        queue.roundtrip(&mut st).map_err(err("roundtrip"))?;
        queue.roundtrip(&mut st).map_err(err("roundtrip"))?;

        Ok(Wl {
            conn,
            queue,
            qh,
            st,
            compositor,
            subcompositor,
            shm,
            layer_shell,
            viewporter,
            frac_mgr,
            card: None,
            menu: None,
            next_pool: 0,
        })
    }

    fn await_configure(&mut self, t: Target) -> Result<(), String> {
        for _ in 0..4 {
            self.queue.roundtrip(&mut self.st).map_err(err("await configure"))?;
            let ev = self.st.ev_ref(t);
            if ev.configured || ev.closed {
                break;
            }
        }
        let ev = self.st.ev_ref(t);
        if ev.closed || !ev.configured {
            return Err("layer surface was not configured".into());
        }
        Ok(())
    }

    // ── card ──

    /// Map the card: `w`×`h` logical, bottom-centre, `margin` above the
    /// bottom edge of the usable area, clickable only inside `input`.
    pub fn show_card(&mut self, w: u32, h: u32, input: (i32, i32, i32, i32), margin: i32) -> Result<(), String> {
        if self.card.is_some() {
            return Ok(());
        }
        self.st.card = SurfEv::default();
        let qh = &self.qh;
        let surface = self.compositor.create_surface(qh, Tag::Card);
        let layer = self.layer_shell.get_layer_surface(
            &surface,
            None,
            zwlr_layer_shell_v1::Layer::Overlay,
            CARD_NAMESPACE.into(),
            qh,
            Target::Card,
        );
        layer.set_size(w, h);
        layer.set_anchor(Anchor::Bottom);
        layer.set_margin(0, 0, margin, 0);
        // 0: keep clear of other surfaces' exclusive zones (a bottom bar).
        layer.set_exclusive_zone(0);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        let region = self.compositor.create_region(qh, ());
        region.add(input.0, input.1, input.2, input.3);
        surface.set_input_region(Some(&region));
        region.destroy();

        let viewport = self.viewporter.as_ref().map(|v| v.get_viewport(&surface, qh, ()));
        let frac = match (&self.frac_mgr, &viewport) {
            (Some(m), Some(_)) => Some(m.get_fractional_scale(&surface, qh, Target::Card)),
            _ => None,
        };
        surface.commit();
        self.card = Some(CardSurf {
            layer,
            frac,
            canvas: Canvas {
                surface,
                tag: Tag::Card,
                viewport,
                pool: None,
                w,
                h,
                frame_pending: None,
                applied_buffer_scale: 1,
                dest: None,
            },
        });
        if let Err(e) = self.await_configure(Target::Card) {
            self.hide_card();
            return Err(e);
        }
        // Scale hints usually follow the configure; one more round trip
        // gets the first frame at the right resolution.
        let _ = self.queue.roundtrip(&mut self.st);
        self.st.card.scale_changed = false;
        Ok(())
    }

    pub fn hide_card(&mut self) {
        if let Some(c) = self.card.take() {
            if let Some(f) = c.frac {
                f.destroy();
            }
            c.layer.destroy();
            c.canvas.destroy();
            let _ = self.conn.flush();
        }
        self.st.card = SurfEv::default();
        if self.st.ptr.is_some_and(|p| p.0 == Target::Card) {
            self.st.ptr = None;
        }
    }

    // ── menu ──

    /// Map the full-output menu surface (transparent, grabs input and the
    /// keyboard). Returns the output's logical size and its x position in
    /// the global layout.
    pub fn open_menu(&mut self) -> Result<((u32, u32), i32), String> {
        if self.subcompositor.is_none() {
            return Err("compositor has no wl_subcompositor".into());
        }
        if let Some(_m) = &self.menu {
            return Ok((self.st.menu.size, self.menu_output_x()));
        }
        self.st.menu = SurfEv::default();
        let qh = &self.qh;
        let bg = self.compositor.create_surface(qh, Tag::MenuBg);
        let layer = self.layer_shell.get_layer_surface(
            &bg,
            None,
            zwlr_layer_shell_v1::Layer::Overlay,
            MENU_NAMESPACE.into(),
            qh,
            Target::Menu,
        );
        layer.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
        layer.set_size(0, 0);
        // -1: cover the whole output, bar included.
        layer.set_exclusive_zone(-1);
        // No keyboard grab: taking focus (exclusive, or on-demand under
        // Hyprland's follow-mouse) moves it off the window and back on close,
        // so the bar's window title flickered on every click. Click outside
        // or on the bar icon again to close.
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        let bg_viewport = self.viewporter.as_ref().map(|v| v.get_viewport(&bg, qh, ()));
        let frac = match (&self.frac_mgr, &bg_viewport) {
            (Some(m), Some(_)) => Some(m.get_fractional_scale(&bg, qh, Target::Menu)),
            _ => None,
        };
        bg.commit();
        self.menu = Some(MenuSurf {
            layer,
            frac,
            bg,
            bg_viewport,
            bg_pool: None,
            sub: None,
            panel: None,
        });
        if let Err(e) = self.await_configure(Target::Menu) {
            self.close_menu();
            return Err(e);
        }
        let (w, h) = self.st.menu.size;
        if w == 0 || h == 0 {
            self.close_menu();
            return Err("menu surface configured with no size".into());
        }

        // Transparent content: one pixel stretched over the output, or a
        // plain 1× buffer when there is no viewporter.
        let format = self.format();
        self.next_pool += 1;
        let id = self.next_pool;
        let m = self.menu.as_mut().expect("menu");
        let (bw, bh) = if m.bg_viewport.is_some() { (1, 1) } else { (w, h) };
        let pool = make_pool(&self.shm, &self.qh, bw, bh, format, id)?;
        if let Some(vp) = &m.bg_viewport {
            vp.set_destination(w as i32, h as i32);
        }
        m.bg.attach(Some(&pool.bufs[0]), 0, 0);
        m.bg.damage_buffer(0, 0, bw as i32, bh as i32);
        m.bg.commit();
        m.bg_pool = Some(pool);
        // Learn which output we landed on (and its scale).
        let _ = self.queue.roundtrip(&mut self.st);
        self.st.menu.scale_changed = false;
        Ok(((w, h), self.menu_output_x()))
    }

    fn menu_output_x(&self) -> i32 {
        self.st
            .outputs
            .iter()
            .find(|(o, _, _)| self.st.menu.entered.contains(&o.id()))
            .map(|(_, _, x)| *x)
            .unwrap_or(0)
    }

    /// Create or move the panel subsurface: `pos` (logical, within the
    /// menu surface), `size` logical, clickable inside `input`
    /// (panel-local).
    pub fn place_panel(&mut self, pos: (i32, i32), size: (u32, u32), input: (i32, i32, i32, i32)) -> Result<(), String> {
        let qh = self.qh.clone();
        let Some(sub_c) = self.subcompositor.clone() else {
            return Err("no wl_subcompositor".into());
        };
        let viewporter = self.viewporter.clone();
        let Some(m) = self.menu.as_mut() else {
            return Err("menu not open".into());
        };
        if m.panel.is_none() {
            let surface = self.compositor.create_surface(&qh, Tag::MenuPanel);
            let sub = sub_c.get_subsurface(&surface, &m.bg, &qh, ());
            sub.set_desync();
            let viewport = viewporter.as_ref().map(|v| v.get_viewport(&surface, &qh, ()));
            m.sub = Some(sub);
            m.panel = Some(Canvas {
                surface,
                tag: Tag::MenuPanel,
                viewport,
                pool: None,
                w: size.0,
                h: size.1,
                frame_pending: None,
                applied_buffer_scale: 1,
                dest: None,
            });
        }
        let panel = m.panel.as_mut().expect("panel");
        panel.w = size.0;
        panel.h = size.1;
        let region = self.compositor.create_region(&qh, ());
        region.add(input.0, input.1, input.2, input.3);
        panel.surface.set_input_region(Some(&region));
        region.destroy();
        if let Some(sub) = &m.sub {
            sub.set_position(pos.0, pos.1);
        }
        // Subsurface position is parent state.
        m.bg.commit();
        self.st.panel_offset = (pos.0 as f64, pos.1 as f64);
        Ok(())
    }

    /// Stop grabbing: let clicks and keys through while the menu fades out.
    pub fn release_menu_input(&mut self) {
        let Some(m) = self.menu.as_ref() else { return };
        let empty = self.compositor.create_region(&self.qh, ());
        m.bg.set_input_region(Some(&empty));
        if let Some(p) = &m.panel {
            p.surface.set_input_region(Some(&empty));
            p.surface.commit();
        }
        empty.destroy();
        m.layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        m.bg.commit();
        let _ = self.conn.flush();
    }

    pub fn close_menu(&mut self) {
        if let Some(m) = self.menu.take() {
            if let Some(p) = m.panel {
                p.destroy();
            }
            if let Some(s) = m.sub {
                s.destroy();
            }
            if let Some(p) = m.bg_pool {
                p.destroy();
            }
            if let Some(v) = m.bg_viewport {
                v.destroy();
            }
            if let Some(f) = m.frac {
                f.destroy();
            }
            m.layer.destroy();
            m.bg.destroy();
            let _ = self.conn.flush();
        }
        self.st.menu = SurfEv::default();
        if self.st.ptr.is_some_and(|p| p.0 == Target::Menu) {
            self.st.ptr = None;
        }
    }

    // ── shared ──

    fn canvas(&self, t: Target) -> Option<&Canvas> {
        match t {
            Target::Card => self.card.as_ref().map(|c| &c.canvas),
            Target::Menu => self.menu.as_ref().and_then(|m| m.panel.as_ref()),
        }
    }

    fn canvas_mut(&mut self, t: Target) -> Option<&mut Canvas> {
        match t {
            Target::Card => self.card.as_mut().map(|c| &mut c.canvas),
            Target::Menu => self.menu.as_mut().and_then(|m| m.panel.as_mut()),
        }
    }

    /// Buffer scale for a target's surface.
    pub fn scale(&self, t: Target) -> f32 {
        let ev = self.st.ev_ref(t);
        let int = || {
            let entered = self
                .st
                .outputs
                .iter()
                .filter(|(o, _, _)| ev.entered.contains(&o.id()))
                .map(|(_, s, _)| *s)
                .max();
            let any = self.st.outputs.iter().map(|(_, s, _)| *s).max();
            ev.pref_int.or(entered).or(any).unwrap_or(1).max(1)
        };
        let viewport = self.canvas(t).is_some_and(|c| c.viewport.is_some());
        match ev.frac {
            Some(f) if f > 0 && viewport => f as f32 / 120.0,
            _ => int() as f32,
        }
    }

    fn format(&self) -> Format {
        if self.st.formats.contains(&wl_shm::Format::Abgr8888) {
            Format::Rgba
        } else {
            Format::Bgra
        }
    }

    pub fn frame_pending(&self, t: Target) -> Option<Instant> {
        self.canvas(t).and_then(|c| c.frame_pending)
    }

    pub fn clear_frame_pending(&mut self, t: Target) {
        if let Some(c) = self.canvas_mut(t) {
            c.frame_pending = None;
        }
        self.st.ev(t).frame_done = false;
    }

    pub fn set_cursor(&self, c: Cursor) {
        if let (Some(dev), Some(serial)) = (&self.st.cursor_dev, self.st.pointer_serial) {
            dev.set_shape(
                serial,
                match c {
                    Cursor::Default => wp_cursor_shape_device_v1::Shape::Default,
                    Cursor::Pointer => wp_cursor_shape_device_v1::Shape::Pointer,
                },
            );
        }
    }

    fn apply_releases(&mut self) {
        let released: Vec<u32> = self.st.released.drain(..).collect();
        for t in [Target::Card, Target::Menu] {
            if let Some(pool) = self.canvas_mut(t).and_then(|c| c.pool.as_mut()) {
                for &tag in &released {
                    if tag >> 1 == pool.id {
                        pool.busy[(tag & 1) as usize] = false;
                    }
                }
            }
        }
    }

    /// Render and commit a frame on `t`. `paint` gets the buffer, its pixel
    /// size, byte order and the exact scale (buffer px per logical px).
    /// Returns false when both buffers are still held by the compositor.
    pub fn draw(
        &mut self,
        t: Target,
        want_frame: bool,
        paint: impl FnOnce(&mut [u8], u32, u32, Format, f32),
    ) -> Result<bool, String> {
        self.apply_releases();
        if !self.st.ev_ref(t).configured {
            return Ok(false);
        }
        let scale = self.scale(t);
        let format = self.format();
        let qh = self.qh.clone();
        let shm = self.shm.clone();
        let mut next_pool = self.next_pool;
        let Some(c) = self.canvas_mut(t) else {
            return Ok(false);
        };

        let (pw, ph, int_scale) = if c.viewport.is_some() {
            (
                (c.w as f32 * scale).round().max(1.0) as u32,
                (c.h as f32 * scale).round().max(1.0) as u32,
                1,
            )
        } else {
            let k = scale.round().max(1.0) as u32;
            (c.w * k, c.h * k, k as i32)
        };
        if c.pool.as_ref().is_none_or(|p| p.w != pw || p.h != ph || p.format != format) {
            if let Some(p) = c.pool.take() {
                p.destroy();
            }
            next_pool += 1;
            c.pool = Some(make_pool(&shm, &qh, pw, ph, format, next_pool)?);
        }
        let pool = c.pool.as_mut().expect("pool");
        let Some(idx) = (0..2).find(|&i| !pool.busy[i]) else {
            self.next_pool = next_pool;
            return Ok(false);
        };
        let one = pw as usize * ph as usize * 4;
        // SAFETY: the mapping holds two frames of `one` bytes; buffer `idx`
        // is not in use by the compositor (no release pending).
        let buf = unsafe { std::slice::from_raw_parts_mut(pool.map.add(idx * one), one) };
        paint(buf, pw, ph, format, pw as f32 / c.w as f32);

        if let Some(vp) = &c.viewport {
            if c.dest != Some((c.w, c.h)) {
                vp.set_destination(c.w as i32, c.h as i32);
                c.dest = Some((c.w, c.h));
            }
        } else if c.applied_buffer_scale != int_scale {
            c.surface.set_buffer_scale(int_scale);
            c.applied_buffer_scale = int_scale;
        }
        c.surface.attach(Some(&pool.bufs[idx]), 0, 0);
        c.surface.damage_buffer(0, 0, pw as i32, ph as i32);
        if want_frame {
            c.surface.frame(&qh, c.tag);
            c.frame_pending = Some(Instant::now());
        }
        c.surface.commit();
        pool.busy[idx] = true;
        self.next_pool = next_pool;
        match self.conn.flush() {
            Ok(()) | Err(WaylandError::Io(_)) => {}
            Err(e) => return Err(format!("flush: {e}")),
        }
        Ok(true)
    }

    /// Before blocking in poll(): dispatch what is queued, flush, and arm
    /// a read. `Ok(None)` means events were dispatched and the caller
    /// should process them before sleeping.
    pub fn prepare(&mut self) -> Result<Option<ReadEventsGuard>, String> {
        if self.queue.dispatch_pending(&mut self.st).map_err(err("dispatch"))? > 0 {
            return Ok(None);
        }
        match self.conn.flush() {
            Ok(()) => {}
            Err(WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(format!("flush: {e}")),
        }
        Ok(self.queue.prepare_read())
    }

    pub fn after_poll(&mut self, guard: ReadEventsGuard, readable: bool) -> Result<(), String> {
        if readable {
            match guard.read() {
                Ok(_) => {}
                Err(WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(format!("read: {e}")),
            }
        } else {
            drop(guard);
        }
        self.queue.dispatch_pending(&mut self.st).map_err(err("dispatch"))?;
        Ok(())
    }
}

impl Drop for Wl {
    fn drop(&mut self) {
        self.hide_card();
        self.close_menu();
    }
}

fn make_pool(shm: &WlShm, qh: &QueueHandle<St>, w: u32, h: u32, format: Format, id: u32) -> Result<Pool, String> {
    let stride = w as usize * 4;
    let one = stride * h as usize;
    let len = one * 2;
    let (fd, map) = shm_alloc(len)?;
    let pool = shm.create_pool(fd.as_fd(), len as i32, qh, ());
    let wl_format = match format {
        Format::Rgba => wl_shm::Format::Abgr8888,
        Format::Bgra => wl_shm::Format::Argb8888,
    };
    let mk = |i: usize| {
        pool.create_buffer(
            (i * one) as i32,
            w as i32,
            h as i32,
            stride as i32,
            wl_format,
            qh,
            (id << 1) | i as u32,
        )
    };
    let bufs = [mk(0), mk(1)];
    Ok(Pool {
        pool,
        _fd: fd,
        map,
        len,
        w,
        h,
        bufs,
        busy: [false; 2],
        id,
        format,
    })
}

/// memfd-backed shared memory; fresh pages read as zero (transparent).
fn shm_alloc(len: usize) -> Result<(OwnedFd, *mut u8), String> {
    let os = |what: &str| format!("{what}: {}", std::io::Error::last_os_error());
    // SAFETY: plain syscalls; every return value is checked and the fd is
    // owned from here on.
    unsafe {
        let raw = libc::memfd_create(c"librepods-popup".as_ptr(), libc::MFD_CLOEXEC);
        if raw < 0 {
            return Err(os("memfd_create"));
        }
        let fd = OwnedFd::from_raw_fd(raw);
        if libc::ftruncate(fd.as_raw_fd(), len as libc::off_t) != 0 {
            return Err(os("ftruncate"));
        }
        let map = libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd.as_raw_fd(),
            0,
        );
        if map == libc::MAP_FAILED {
            return Err(os("mmap"));
        }
        Ok((fd, map as *mut u8))
    }
}

// ─────────────────────────────── dispatch ───────────────────────────────

impl Dispatch<WlRegistry, GlobalListContents> for St {
    fn event(_: &mut Self, _: &WlRegistry, _: wayland_client::protocol::wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<WlShm, ()> for St {
    fn event(st: &mut Self, _: &WlShm, ev: wl_shm::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_shm::Event::Format { format: WEnum::Value(f) } = ev {
            st.formats.push(f);
        }
    }
}

impl Dispatch<WlOutput, ()> for St {
    fn event(st: &mut Self, o: &WlOutput, ev: wl_output::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        let Some(e) = st.outputs.iter_mut().find(|(x, _, _)| x == o) else {
            return;
        };
        match ev {
            wl_output::Event::Scale { factor } => {
                e.1 = factor;
                st.card.scale_changed = true;
                st.menu.scale_changed = true;
            }
            wl_output::Event::Geometry { x, .. } => e.2 = x,
            _ => {}
        }
    }
}

impl Dispatch<WlSurface, Tag> for St {
    fn event(st: &mut Self, _: &WlSurface, ev: wl_surface::Event, tag: &Tag, _: &Connection, _: &QueueHandle<Self>) {
        // Scale follows the layer surfaces; the panel inherits the menu's.
        if *tag == Tag::MenuPanel {
            return;
        }
        let s = st.ev(tag.target());
        match ev {
            wl_surface::Event::Enter { output } => {
                s.entered.push(output.id());
                s.scale_changed = true;
            }
            wl_surface::Event::Leave { output } => {
                s.entered.retain(|id| *id != output.id());
                s.scale_changed = true;
            }
            wl_surface::Event::PreferredBufferScale { factor } => {
                s.pref_int = Some(factor);
                s.scale_changed = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<WpFractionalScaleV1, Target> for St {
    fn event(st: &mut Self, _: &WpFractionalScaleV1, ev: wp_fractional_scale_v1::Event, t: &Target, _: &Connection, _: &QueueHandle<Self>) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = ev {
            let s = st.ev(*t);
            s.frac = Some(scale);
            s.scale_changed = true;
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, Target> for St {
    fn event(st: &mut Self, layer: &ZwlrLayerSurfaceV1, ev: zwlr_layer_surface_v1::Event, t: &Target, _: &Connection, _: &QueueHandle<Self>) {
        let s = st.ev(*t);
        match ev {
            zwlr_layer_surface_v1::Event::Configure { serial, width, height } => {
                layer.ack_configure(serial);
                s.configured = true;
                s.size = (width, height);
            }
            zwlr_layer_surface_v1::Event::Closed => s.closed = true,
            _ => {}
        }
    }
}

impl Dispatch<WlBuffer, u32> for St {
    fn event(st: &mut Self, _: &WlBuffer, ev: wl_buffer::Event, tag: &u32, _: &Connection, _: &QueueHandle<Self>) {
        if let wl_buffer::Event::Release = ev {
            st.released.push(*tag);
        }
    }
}

impl Dispatch<WlCallback, Tag> for St {
    fn event(st: &mut Self, _: &WlCallback, ev: wl_callback::Event, tag: &Tag, _: &Connection, _: &QueueHandle<Self>) {
        if let wl_callback::Event::Done { .. } = ev {
            st.ev(tag.target()).frame_done = true;
        }
    }
}

impl Dispatch<WlSeat, ()> for St {
    fn event(st: &mut Self, seat: &WlSeat, ev: wl_seat::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        let wl_seat::Event::Capabilities { capabilities: WEnum::Value(caps) } = ev else {
            return;
        };
        let has_ptr = caps.contains(wl_seat::Capability::Pointer);
        if has_ptr && st.pointer.is_none() {
            let p = seat.get_pointer(qh, ());
            if let Some(m) = &st.cursor_mgr {
                st.cursor_dev = Some(m.get_pointer(&p, qh, ()));
            }
            st.pointer = Some(p);
        } else if !has_ptr && let Some(p) = st.pointer.take() {
            if let Some(d) = st.cursor_dev.take() {
                d.destroy();
            }
            if p.version() >= 3 {
                p.release();
            }
        }
        let has_touch = caps.contains(wl_seat::Capability::Touch);
        if has_touch && st.touch.is_none() {
            st.touch = Some(seat.get_touch(qh, ()));
        } else if !has_touch
            && let Some(t) = st.touch.take()
            && t.version() >= 3
        {
            t.release();
        }
        let has_kbd = caps.contains(wl_seat::Capability::Keyboard);
        if has_kbd && st.keyboard.is_none() {
            st.keyboard = Some(seat.get_keyboard(qh, ()));
        } else if !has_kbd
            && let Some(k) = st.keyboard.take()
            && k.version() >= 3
        {
            k.release();
        }
    }
}

fn tag_of(s: &WlSurface) -> Option<Tag> {
    s.data::<Tag>().copied()
}

impl Dispatch<WlPointer, ()> for St {
    fn event(st: &mut Self, _: &WlPointer, ev: wl_pointer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match ev {
            wl_pointer::Event::Enter {
                serial,
                surface,
                surface_x,
                surface_y,
            } => {
                let Some(tag) = tag_of(&surface) else { return };
                let (ox, oy) = st.offset(tag);
                let t = tag.target();
                st.pointer_serial = Some(serial);
                st.ptr = Some((t, ox, oy));
                if let Some(d) = &st.cursor_dev {
                    d.set_shape(serial, wp_cursor_shape_device_v1::Shape::Default);
                }
                st.input.push(Input::Enter(t, surface_x + ox, surface_y + oy));
            }
            wl_pointer::Event::Leave { surface, .. } => {
                if let Some(tag) = tag_of(&surface) {
                    st.input.push(Input::Leave(tag.target()));
                }
                st.ptr = None;
            }
            wl_pointer::Event::Motion { surface_x, surface_y, .. } => {
                if let Some((t, ox, oy)) = st.ptr {
                    st.input.push(Input::Motion(t, surface_x + ox, surface_y + oy));
                }
            }
            wl_pointer::Event::Button {
                button,
                state: WEnum::Value(s),
                ..
            } => {
                if let Some((t, _, _)) = st.ptr {
                    st.input.push(match s {
                        wl_pointer::ButtonState::Pressed => Input::Press(t, button),
                        _ => Input::Release(t, button),
                    });
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WlTouch, ()> for St {
    fn event(st: &mut Self, _: &WlTouch, ev: wl_touch::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match ev {
            wl_touch::Event::Down { id, surface, x, y, .. } if st.touch_focus.is_none() => {
                let Some(tag) = tag_of(&surface) else { return };
                let (ox, oy) = st.offset(tag);
                st.touch_focus = Some((id, tag.target(), ox, oy));
                st.input.push(Input::TouchDown(tag.target(), x + ox, y + oy));
            }
            wl_touch::Event::Motion { id, x, y, .. } => {
                if let Some((tid, t, ox, oy)) = st.touch_focus
                    && tid == id
                {
                    st.input.push(Input::TouchMotion(t, x + ox, y + oy));
                }
            }
            wl_touch::Event::Up { id, .. } => {
                if let Some((tid, t, _, _)) = st.touch_focus
                    && tid == id
                {
                    st.touch_focus = None;
                    st.input.push(Input::TouchUp(t));
                }
            }
            wl_touch::Event::Cancel => {
                st.touch_focus = None;
                st.input.push(Input::TouchCancel);
            }
            _ => {}
        }
    }
}

impl Dispatch<WlKeyboard, ()> for St {
    fn event(st: &mut Self, _: &WlKeyboard, ev: wl_keyboard::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match ev {
            // Only Escape matters, and evdev codes need no keymap; the fd
            // is simply closed.
            wl_keyboard::Event::Keymap { fd, .. } => drop(fd),
            wl_keyboard::Event::Key {
                key,
                state: WEnum::Value(wl_keyboard::KeyState::Pressed),
                ..
            } => st.input.push(Input::Key(key)),
            _ => {}
        }
    }
}

delegate_noop!(St: ignore WlCompositor);
delegate_noop!(St: ignore WlSubcompositor);
delegate_noop!(St: ignore WlSubsurface);
delegate_noop!(St: ignore WlShmPool);
delegate_noop!(St: ignore WlRegion);
delegate_noop!(St: ignore ZwlrLayerShellV1);
delegate_noop!(St: ignore WpViewporter);
delegate_noop!(St: ignore WpViewport);
delegate_noop!(St: ignore WpFractionalScaleManagerV1);
delegate_noop!(St: ignore WpCursorShapeManagerV1);
delegate_noop!(St: ignore WpCursorShapeDeviceV1);
