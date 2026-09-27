//! wlr-layer-shell overlay surface on raw wl_shm buffers.
//!
//! Adapted from podctl's `src/bin/popup/backend/wl_layer.rs`
//! (https://github.com/Rockykln/podctl, MIT OR Apache-2.0,
//! © 2026 pods contributors): memfd-backed double-buffered wl_shm pool,
//! overlay layer anchored to the bottom edge, no keyboard interactivity.
//!
//! Additions here: the connection stays open and is driven from the
//! caller's poll loop (no blocking dispatch), pointer and touch input,
//! an input region limited to the card, frame callbacks for pacing,
//! fractional scaling via wp_fractional_scale_v1 + wp_viewporter (integer
//! `preferred_buffer_scale` / output scale otherwise), cursor shapes, and
//! rendering straight into the shm buffer when the compositor takes
//! ABGR8888.

use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::time::Instant;

use wayland_client::backend::{ObjectId, ReadEventsGuard, WaylandError};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{
    wl_buffer::{self, WlBuffer},
    wl_callback::{self, WlCallback},
    wl_compositor::WlCompositor,
    wl_output::{self, WlOutput},
    wl_pointer::{self, WlPointer},
    wl_region::WlRegion,
    wl_registry::WlRegistry,
    wl_seat::{self, WlSeat},
    wl_shm::{self, WlShm},
    wl_shm_pool::WlShmPool,
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

pub const NAMESPACE: &str = "librepods-popup";

/// Linux input event codes.
pub const BTN_LEFT: u32 = 0x110;
pub const BTN_RIGHT: u32 = 0x111;

#[derive(Clone, Copy, Debug)]
pub enum Input {
    Enter(f64, f64),
    Motion(f64, f64),
    Leave,
    Press(u32),
    Release(u32),
    TouchDown(f64, f64),
    TouchMotion(f64, f64),
    TouchUp,
    TouchCancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cursor {
    Default,
    Pointer,
}

/// Event sink for the dispatch handlers; the app reads it after each
/// dispatch.
#[derive(Default)]
pub struct St {
    formats: Vec<wl_shm::Format>,
    seat: Option<WlSeat>,
    pointer: Option<WlPointer>,
    touch: Option<WlTouch>,
    cursor_mgr: Option<WpCursorShapeManagerV1>,
    cursor_dev: Option<WpCursorShapeDeviceV1>,
    pointer_serial: Option<u32>,
    touch_id: Option<i32>,
    outputs: Vec<(WlOutput, i32)>,
    pub input: Vec<Input>,

    // Current surface.
    configured: bool,
    pub closed: bool,
    frac: Option<u32>,
    pref_int: Option<i32>,
    entered: Vec<ObjectId>,
    pub scale_changed: bool,
    pub frame_done: bool,
    released: Vec<u32>,
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
    generation: u32,
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

struct Surf {
    surface: WlSurface,
    layer: ZwlrLayerSurfaceV1,
    viewport: Option<WpViewport>,
    frac: Option<WpFractionalScaleV1>,
    w: u32,
    h: u32,
    pool: Option<Pool>,
    frame_pending: Option<Instant>,
    applied_buffer_scale: i32,
    viewport_set: bool,
}

pub struct Wl {
    conn: Connection,
    queue: EventQueue<St>,
    qh: QueueHandle<St>,
    pub st: St,
    compositor: WlCompositor,
    shm: WlShm,
    layer_shell: ZwlrLayerShellV1,
    viewporter: Option<WpViewporter>,
    frac_mgr: Option<WpFractionalScaleManagerV1>,
    surf: Option<Surf>,
    generation: u32,
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
                st.outputs.push((o, 1));
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
            shm,
            layer_shell,
            viewporter,
            frac_mgr,
            surf: None,
            generation: 0,
        })
    }

    /// Map the overlay: `w`×`h` logical, bottom-centre, `margin` above the
    /// bottom edge of the usable area, clickable only inside `input`.
    pub fn show(&mut self, w: u32, h: u32, input: (i32, i32, i32, i32), margin: i32) -> Result<(), String> {
        if self.surf.is_some() {
            return Ok(());
        }
        let qh = &self.qh;
        self.st.configured = false;
        self.st.closed = false;
        self.st.frac = None;
        self.st.pref_int = None;
        self.st.entered.clear();
        self.st.frame_done = false;

        let surface = self.compositor.create_surface(qh, ());
        let layer = self.layer_shell.get_layer_surface(
            &surface,
            None,
            zwlr_layer_shell_v1::Layer::Overlay,
            NAMESPACE.into(),
            qh,
            (),
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
            (Some(m), Some(_)) => Some(m.get_fractional_scale(&surface, qh, ())),
            _ => None,
        };
        surface.commit();

        self.surf = Some(Surf {
            surface,
            layer,
            viewport,
            frac,
            w,
            h,
            pool: None,
            frame_pending: None,
            applied_buffer_scale: 1,
            viewport_set: false,
        });

        for _ in 0..4 {
            self.queue.roundtrip(&mut self.st).map_err(err("await configure"))?;
            if self.st.configured || self.st.closed {
                break;
            }
        }
        if self.st.closed || !self.st.configured {
            self.hide();
            return Err("layer surface was not configured".into());
        }
        // Scale hints usually follow the configure; one more round trip
        // gets the first frame at the right resolution.
        let _ = self.queue.roundtrip(&mut self.st);
        self.st.scale_changed = false;
        Ok(())
    }

    pub fn hide(&mut self) {
        if let Some(s) = self.surf.take() {
            if let Some(p) = s.pool {
                p.destroy();
            }
            if let Some(v) = s.viewport {
                v.destroy();
            }
            if let Some(f) = s.frac {
                f.destroy();
            }
            s.layer.destroy();
            s.surface.destroy();
            let _ = self.conn.flush();
        }
        self.st.configured = false;
        self.st.released.clear();
        self.st.frame_done = false;
    }

    /// Buffer scale for the current surface.
    pub fn scale(&self) -> f32 {
        let int = || {
            let entered = self
                .st
                .outputs
                .iter()
                .filter(|(o, _)| self.st.entered.contains(&o.id()))
                .map(|(_, s)| *s)
                .max();
            let any = self.st.outputs.iter().map(|(_, s)| *s).max();
            self.st.pref_int.or(entered).or(any).unwrap_or(1).max(1)
        };
        match self.surf.as_ref() {
            Some(s) if s.viewport.is_some() => match self.st.frac {
                Some(f) if f > 0 => f as f32 / 120.0,
                _ => int() as f32,
            },
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

    pub fn frame_pending(&self) -> Option<Instant> {
        self.surf.as_ref().and_then(|s| s.frame_pending)
    }

    pub fn clear_frame_pending(&mut self) {
        if let Some(s) = self.surf.as_mut() {
            s.frame_pending = None;
        }
        self.st.frame_done = false;
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
        let Some(pool) = self.surf.as_mut().and_then(|s| s.pool.as_mut()) else {
            self.st.released.clear();
            return;
        };
        for tag in self.st.released.drain(..) {
            if tag >> 1 == pool.generation {
                pool.busy[(tag & 1) as usize] = false;
            }
        }
    }

    /// Render and commit a frame. `paint` gets the buffer, its pixel size,
    /// byte order and the exact scale (buffer px per logical px). Returns
    /// false when both buffers are still held by the compositor.
    pub fn draw(&mut self, want_frame: bool, paint: impl FnOnce(&mut [u8], u32, u32, Format, f32)) -> Result<bool, String> {
        self.apply_releases();
        if !self.st.configured {
            return Ok(false);
        }
        let scale = self.scale();
        let format = self.format();
        let qh = self.qh.clone();
        let Some(surf) = self.surf.as_mut() else {
            return Ok(false);
        };

        let (pw, ph, int_scale) = if surf.viewport.is_some() {
            (
                (surf.w as f32 * scale).round().max(1.0) as u32,
                (surf.h as f32 * scale).round().max(1.0) as u32,
                1,
            )
        } else {
            let k = scale.round().max(1.0) as u32;
            (surf.w * k, surf.h * k, k as i32)
        };

        if surf.pool.as_ref().is_none_or(|p| p.w != pw || p.h != ph || p.format != format) {
            if let Some(p) = surf.pool.take() {
                p.destroy();
            }
            self.generation = self.generation.wrapping_add(1) & 0x7FFF_FFFF;
            surf.pool = Some(make_pool(&self.shm, &qh, pw, ph, format, self.generation)?);
        }
        let pool = surf.pool.as_mut().expect("pool");
        let Some(idx) = (0..2).find(|&i| !pool.busy[i]) else {
            return Ok(false);
        };
        let one = pw as usize * ph as usize * 4;
        // SAFETY: the mapping holds two frames of `one` bytes; buffer `idx`
        // is not in use by the compositor (no release pending).
        let buf = unsafe { std::slice::from_raw_parts_mut(pool.map.add(idx * one), one) };
        paint(buf, pw, ph, format, pw as f32 / surf.w as f32);

        if let Some(vp) = &surf.viewport {
            if !surf.viewport_set {
                vp.set_destination(surf.w as i32, surf.h as i32);
                surf.viewport_set = true;
            }
        } else if surf.applied_buffer_scale != int_scale {
            surf.surface.set_buffer_scale(int_scale);
            surf.applied_buffer_scale = int_scale;
        }
        surf.surface.attach(Some(&pool.bufs[idx]), 0, 0);
        surf.surface.damage_buffer(0, 0, pw as i32, ph as i32);
        if want_frame {
            surf.surface.frame(&qh, ());
            surf.frame_pending = Some(Instant::now());
        }
        surf.surface.commit();
        pool.busy[idx] = true;
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
        self.hide();
    }
}

fn make_pool(shm: &WlShm, qh: &QueueHandle<St>, w: u32, h: u32, format: Format, generation: u32) -> Result<Pool, String> {
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
            (generation << 1) | i as u32,
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
        generation,
        format,
    })
}

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
        if let wl_output::Event::Scale { factor } = ev
            && let Some(e) = st.outputs.iter_mut().find(|(x, _)| x == o)
        {
            e.1 = factor;
            st.scale_changed = true;
        }
    }
}

impl Dispatch<WlSurface, ()> for St {
    fn event(st: &mut Self, _: &WlSurface, ev: wl_surface::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match ev {
            wl_surface::Event::Enter { output } => {
                st.entered.push(output.id());
                st.scale_changed = true;
            }
            wl_surface::Event::Leave { output } => {
                st.entered.retain(|id| *id != output.id());
                st.scale_changed = true;
            }
            wl_surface::Event::PreferredBufferScale { factor } => {
                st.pref_int = Some(factor);
                st.scale_changed = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<WpFractionalScaleV1, ()> for St {
    fn event(st: &mut Self, _: &WpFractionalScaleV1, ev: wp_fractional_scale_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = ev {
            st.frac = Some(scale);
            st.scale_changed = true;
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for St {
    fn event(st: &mut Self, layer: &ZwlrLayerSurfaceV1, ev: zwlr_layer_surface_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match ev {
            zwlr_layer_surface_v1::Event::Configure { serial, .. } => {
                layer.ack_configure(serial);
                st.configured = true;
            }
            zwlr_layer_surface_v1::Event::Closed => st.closed = true,
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

impl Dispatch<WlCallback, ()> for St {
    fn event(st: &mut Self, _: &WlCallback, ev: wl_callback::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_callback::Event::Done { .. } = ev {
            st.frame_done = true;
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
        } else if !has_touch && let Some(t) = st.touch.take()
            && t.version() >= 3
        {
            t.release();
        }
    }
}

impl Dispatch<WlPointer, ()> for St {
    fn event(st: &mut Self, _: &WlPointer, ev: wl_pointer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match ev {
            wl_pointer::Event::Enter {
                serial,
                surface_x,
                surface_y,
                ..
            } => {
                st.pointer_serial = Some(serial);
                if let Some(d) = &st.cursor_dev {
                    d.set_shape(serial, wp_cursor_shape_device_v1::Shape::Default);
                }
                st.input.push(Input::Enter(surface_x, surface_y));
            }
            wl_pointer::Event::Leave { .. } => {
                st.pointer_serial = None;
                st.input.push(Input::Leave);
            }
            wl_pointer::Event::Motion { surface_x, surface_y, .. } => {
                st.input.push(Input::Motion(surface_x, surface_y));
            }
            wl_pointer::Event::Button {
                button,
                state: WEnum::Value(s),
                ..
            } => st.input.push(match s {
                wl_pointer::ButtonState::Pressed => Input::Press(button),
                _ => Input::Release(button),
            }),
            _ => {}
        }
    }
}

impl Dispatch<WlTouch, ()> for St {
    fn event(st: &mut Self, _: &WlTouch, ev: wl_touch::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match ev {
            wl_touch::Event::Down { id, x, y, .. } if st.touch_id.is_none() => {
                st.touch_id = Some(id);
                st.input.push(Input::TouchDown(x, y));
            }
            wl_touch::Event::Motion { id, x, y, .. } if st.touch_id == Some(id) => {
                st.input.push(Input::TouchMotion(x, y));
            }
            wl_touch::Event::Up { id, .. } if st.touch_id == Some(id) => {
                st.touch_id = None;
                st.input.push(Input::TouchUp);
            }
            wl_touch::Event::Cancel => {
                st.touch_id = None;
                st.input.push(Input::TouchCancel);
            }
            _ => {}
        }
    }
}

delegate_noop!(St: ignore WlCompositor);
delegate_noop!(St: ignore WlShmPool);
delegate_noop!(St: ignore WlRegion);
delegate_noop!(St: ignore ZwlrLayerShellV1);
delegate_noop!(St: ignore WpViewporter);
delegate_noop!(St: ignore WpViewport);
delegate_noop!(St: ignore WpFractionalScaleManagerV1);
delegate_noop!(St: ignore WpCursorShapeManagerV1);
delegate_noop!(St: ignore WpCursorShapeDeviceV1);
