//! Shared look: one font family at fixed logical sizes (nwg's `high-dpi`
//! feature scales both fonts and coordinates, so layouts hold at 100–200 %).

use nwg::{ControlHandle, NwgError};

#[derive(Default)]
pub struct Fonts {
    pub normal: nwg::Font,
    pub small: nwg::Font,
    pub bold: nwg::Font,
    pub head: nwg::Font,
    pub mono: nwg::Font,
}

impl Fonts {
    pub fn build(&mut self) -> Result<(), NwgError> {
        nwg::Font::builder()
            .family("Segoe UI")
            .size(17)
            .build(&mut self.normal)?;
        nwg::Font::builder()
            .family("Segoe UI")
            .size(15)
            .build(&mut self.small)?;
        nwg::Font::builder()
            .family("Segoe UI Semibold")
            .size(17)
            .build(&mut self.bold)?;
        nwg::Font::builder()
            .family("Segoe UI Semibold")
            .size(24)
            .build(&mut self.head)?;
        nwg::Font::builder()
            .family("Consolas")
            .size(15)
            .build(&mut self.mono)?;
        // Everything built without an explicit font uses this one.
        let mut global = nwg::Font::default();
        nwg::Font::builder()
            .family("Segoe UI")
            .size(17)
            .build(&mut global)?;
        nwg::Font::set_global_default(Some(global));
        Ok(())
    }
}

pub fn label<P: Into<ControlHandle>>(
    out: &mut nwg::Label,
    parent: P,
    text: &str,
    pos: (i32, i32),
    size: (i32, i32),
    font: &nwg::Font,
) -> Result<(), NwgError> {
    nwg::Label::builder()
        .text(text)
        .position(pos)
        .size(size)
        .font(Some(font))
        .v_align(nwg::VTextAlign::Top)
        .parent(parent)
        .build(out)
}

pub fn button<P: Into<ControlHandle>>(
    out: &mut nwg::Button,
    parent: P,
    text: &str,
    pos: (i32, i32),
    size: (i32, i32),
) -> Result<(), NwgError> {
    nwg::Button::builder()
        .text(text)
        .position(pos)
        .size(size)
        .parent(parent)
        .build(out)
}

pub fn check<P: Into<ControlHandle>>(
    out: &mut nwg::CheckBox,
    parent: P,
    text: &str,
    pos: (i32, i32),
    size: (i32, i32),
) -> Result<(), NwgError> {
    nwg::CheckBox::builder()
        .text(text)
        .position(pos)
        .size(size)
        .parent(parent)
        .build(out)
}

pub fn checked(c: &nwg::CheckBox) -> bool {
    c.check_state() == nwg::CheckBoxState::Checked
}

pub fn set_checked(c: &nwg::CheckBox, v: bool) {
    c.set_check_state(if v {
        nwg::CheckBoxState::Checked
    } else {
        nwg::CheckBoxState::Unchecked
    });
}

#[link(name = "gdi32")]
extern "system" {
    fn SetBkColor(hdc: *mut std::ffi::c_void, color: u32) -> u32;
    fn GetStockObject(i: i32) -> *mut std::ffi::c_void;
}

/// Tab pages are white, but labels / checkboxes / read-only edits on them ask
/// their parent for a colour and default to the gray dialog colour. Answer
/// that request (WM_CTLCOLORSTATIC) with white so they blend into the page.
pub fn white_statics(page: &ControlHandle, id: usize) -> Result<nwg::RawEventHandler, NwgError> {
    const WM_CTLCOLORSTATIC: u32 = 0x0138;
    nwg::bind_raw_event_handler(page, 0x1_0000 + id, |_hwnd, msg, wparam, _lparam| {
        if msg == WM_CTLCOLORSTATIC {
            unsafe {
                SetBkColor(wparam as *mut std::ffi::c_void, 0x00FF_FFFF);
                Some(GetStockObject(0 /* WHITE_BRUSH */) as isize)
            }
        } else {
            None
        }
    })
}

#[repr(C)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[link(name = "gdi32")]
extern "system" {
    fn CreateSolidBrush(color: u32) -> *mut std::ffi::c_void;
    fn SetTextColor(hdc: *mut std::ffi::c_void, color: u32) -> u32;
}
#[link(name = "user32")]
extern "system" {
    fn FillRect(hdc: *mut std::ffi::c_void, rect: *const Rect, brush: *mut std::ffi::c_void)
        -> i32;
    fn GetClientRect(hwnd: *mut std::ffi::c_void, rect: *mut Rect) -> i32;
    fn GetClassLongPtrW(hwnd: *mut std::ffi::c_void, index: i32) -> usize;
    fn RedrawWindow(
        hwnd: *mut std::ffi::c_void,
        rect: *const Rect,
        rgn: *mut std::ffi::c_void,
        flags: u32,
    ) -> i32;
}

/// Paints a coloured band across the top `height` logical px of `window`, behind the header
/// controls, so the bypass state reads at a glance. `colours` holds (band, headline text) as
/// COLORREFs; change it, then call `redraw_band`. Labels and icons on the window take the band
/// colour as their background; `headline` also takes the text colour.
pub fn state_band(
    window: &ControlHandle,
    height: i32,
    colours: std::rc::Rc<std::cell::Cell<(u32, u32)>>,
    headline: &ControlHandle,
) -> Result<nwg::RawEventHandler, NwgError> {
    const WM_ERASEBKGND: u32 = 0x0014;
    const WM_CTLCOLORSTATIC: u32 = 0x0138;
    const GCLP_HBRBACKGROUND: i32 = -10;
    let headline = headline.hwnd().map_or(0, |h| h as isize);
    let brushes: std::cell::RefCell<Vec<(u32, *mut std::ffi::c_void)>> = Default::default();
    let brush = move |c: u32| {
        let mut b = brushes.borrow_mut();
        if let Some(&(_, h)) = b.iter().find(|(k, _)| *k == c) {
            return h;
        }
        let h = unsafe { CreateSolidBrush(c) }; // a handful, kept for the app's lifetime
        b.push((c, h));
        h
    };
    nwg::bind_raw_event_handler(window, 0x2_0000, move |hwnd, msg, wparam, lparam| unsafe {
        let hwnd = hwnd as *mut std::ffi::c_void;
        let hdc = wparam as *mut std::ffi::c_void;
        match msg {
            WM_ERASEBKGND => {
                let mut rc = Rect {
                    left: 0,
                    top: 0,
                    right: 0,
                    bottom: 0,
                };
                GetClientRect(hwnd, &mut rc);
                FillRect(
                    hdc,
                    &rc,
                    GetClassLongPtrW(hwnd, GCLP_HBRBACKGROUND) as *mut std::ffi::c_void,
                );
                rc.bottom = px(height as u32) as i32;
                FillRect(hdc, &rc, brush(colours.get().0));
                Some(1)
            }
            WM_CTLCOLORSTATIC => {
                let (band, text) = colours.get();
                SetBkColor(hdc, band);
                if lparam == headline {
                    SetTextColor(hdc, text);
                }
                Some(brush(band) as isize)
            }
            _ => None,
        }
    })
}

#[link(name = "user32")]
extern "system" {
    fn GetWindowRect(hwnd: *mut std::ffi::c_void, rect: *mut Rect) -> i32;
    fn GetWindowDC(hwnd: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
    fn ReleaseDC(hwnd: *mut std::ffi::c_void, hdc: *mut std::ffi::c_void) -> i32;
    fn ClientToScreen(hwnd: *mut std::ffi::c_void, pt: *mut [i32; 2]) -> i32;
    fn SendMessageW(hwnd: *mut std::ffi::c_void, msg: u32, wparam: usize, lparam: isize) -> isize;
}
#[link(name = "gdi32")]
extern "system" {
    fn ExcludeClipRect(hdc: *mut std::ffi::c_void, l: i32, t: i32, r: i32, b: i32) -> i32;
}

thread_local! {
    static BRUSHES: std::cell::RefCell<Vec<(u32, usize)>> = Default::default();
}

/// A solid brush for `c`, created once and kept for the app's lifetime.
fn solid(c: u32) -> *mut std::ffi::c_void {
    BRUSHES.with(|b| {
        let mut b = b.borrow_mut();
        if let Some(&(_, h)) = b.iter().find(|(k, _)| *k == c) {
            return h as *mut std::ffi::c_void;
        }
        let h = unsafe { CreateSolidBrush(c) };
        b.push((c, h as usize));
        h
    })
}

/// nwg labels keep the space below their text as a frame (non-client area) and paint it in a
/// fixed system grey, which shows as a strip once a label is resized. Paint it in `colour()`
/// instead: white on the tab pages, the band colour in the header.
pub fn label_frame_fill(
    label: &ControlHandle,
    id: usize,
    colour: impl Fn() -> u32 + 'static,
) -> Result<nwg::RawEventHandler, NwgError> {
    const WM_NCPAINT: u32 = 0x0085;
    nwg::bind_raw_event_handler(label, 0x3_0000 + id, move |hwnd, msg, _w, _l| unsafe {
        if msg != WM_NCPAINT {
            return None;
        }
        let hwnd = hwnd as *mut std::ffi::c_void;
        let (mut wr, mut cr) = (
            Rect {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            Rect {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
        );
        GetWindowRect(hwnd, &mut wr);
        GetClientRect(hwnd, &mut cr);
        let mut origin = [0i32; 2];
        ClientToScreen(hwnd, &mut origin);
        let (ox, oy) = (origin[0] - wr.left, origin[1] - wr.top);
        let dc = GetWindowDC(hwnd);
        ExcludeClipRect(dc, ox, oy, ox + cr.right, oy + cr.bottom);
        let all = Rect {
            left: 0,
            top: 0,
            right: wr.right - wr.left,
            bottom: wr.bottom - wr.top,
        };
        FillRect(dc, &all, solid(colour()));
        ReleaseDC(hwnd, dc);
        Some(0)
    })
}

/// Turns painting of `window` (and everything in it) off or back on. Used around a re-layout
/// so the window repaints once instead of once per moved control.
pub fn set_redraw(window: &nwg::Window, on: bool) {
    const WM_SETREDRAW: u32 = 0x000B;
    if let Some(h) = window.handle.hwnd() {
        unsafe { SendMessageW(h as *mut std::ffi::c_void, WM_SETREDRAW, on as usize, 0) };
    }
}

/// Repaints the whole of `window` and its controls, frames included.
pub fn redraw_all(window: &nwg::Window) {
    const RDW_INVALIDATE: u32 = 0x1;
    const RDW_ERASE: u32 = 0x4;
    const RDW_ALLCHILDREN: u32 = 0x80;
    const RDW_FRAME: u32 = 0x400;
    if let Some(h) = window.handle.hwnd() {
        unsafe {
            RedrawWindow(
                h as *mut std::ffi::c_void,
                std::ptr::null(),
                std::ptr::null_mut(),
                RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN | RDW_FRAME,
            )
        };
    }
}

/// Repaints the band area of `window` (and the controls on it) after its colours change.
pub fn redraw_band(window: &nwg::Window, height: i32) {
    const RDW_INVALIDATE: u32 = 0x1;
    const RDW_ERASE: u32 = 0x4;
    const RDW_ALLCHILDREN: u32 = 0x80;
    if let Some(h) = window.handle.hwnd() {
        let h = h as *mut std::ffi::c_void;
        unsafe {
            let mut rc = Rect {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            GetClientRect(h, &mut rc);
            rc.bottom = px(height as u32) as i32;
            RedrawWindow(
                h,
                &rc,
                std::ptr::null_mut(),
                RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN | 0x400, /* RDW_FRAME */
            );
        }
    }
}

pub fn set_if(l: &nwg::Label, s: &str) {
    if l.text() != s {
        l.set_text(s);
    }
}

/// Shows %LOCALAPPDATA% / %USERPROFILE% instead of the long absolute prefix.
pub fn short_path(p: &std::path::Path) -> String {
    let s = p.display().to_string();
    for var in ["LOCALAPPDATA", "APPDATA", "USERPROFILE"] {
        if let Ok(v) = std::env::var(var) {
            if !v.is_empty() && s.to_lowercase().starts_with(&v.to_lowercase()) {
                return format!("%{var}%{}", &s[v.len()..]);
            }
        }
    }
    s
}

// ----------------------------------------------------------------- icons from memory

#[link(name = "user32")]
extern "system" {
    fn CreateIconFromResourceEx(
        bits: *const u8,
        size: u32,
        icon: i32,
        ver: u32,
        cx: i32,
        cy: i32,
        flags: u32,
    ) -> *mut std::ffi::c_void;
}

/// Logical px -> physical px at the current scaling.
pub fn px(logical: u32) -> u32 {
    (logical as f64 * nwg::scale_factor()).round() as u32
}

/// Builds an icon from an embedded .ico, using the entry closest to `size_px`
/// (smallest entry >= size, else the largest). No file on disk needed.
pub fn icon_from_ico(ico: &[u8], size_px: u32) -> nwg::Icon {
    let rd16 = |o: usize| u16::from_le_bytes([ico[o], ico[o + 1]]) as usize;
    let rd32 = |o: usize| u32::from_le_bytes([ico[o], ico[o + 1], ico[o + 2], ico[o + 3]]) as usize;
    let count = rd16(4);
    let mut best: Option<(u32, usize, usize)> = None; // (w, offset, len)
    for i in 0..count {
        let e = 6 + 16 * i;
        let w = if ico[e] == 0 { 256 } else { ico[e] as u32 };
        let cand = (w, rd32(e + 12), rd32(e + 8));
        best = Some(match best {
            None => cand,
            Some(b) => {
                let better = if w >= size_px {
                    b.0 < size_px || w < b.0
                } else {
                    b.0 < size_px && w > b.0
                };
                if better {
                    cand
                } else {
                    b
                }
            }
        });
    }
    let mut icon = nwg::Icon::default();
    if let Some((_, off, len)) = best {
        let h = unsafe {
            CreateIconFromResourceEx(
                ico[off..].as_ptr(),
                len as u32,
                1,
                0x0003_0000,
                size_px as i32,
                size_px as i32,
                0,
            )
        };
        icon.handle = h as _;
    }
    icon
}

macro_rules! glyph {
    ($n:literal) => {
        include_bytes!(concat!("../icons/ui/", $n, ".ico"))
    };
}

/// Monotone UI glyphs, 16 logical px.
#[derive(Default)]
pub struct Glyphs {
    pub status: nwg::Icon,
    pub flags: nwg::Icon,
    pub log: nwg::Icon,
    pub settings: nwg::Icon,
    pub power: nwg::Icon,
    pub check: nwg::Icon,
    pub tune: nwg::Icon,
    pub help: nwg::Icon,
    pub apply: nwg::Icon,
    pub undo: nwg::Icon,
    pub plus: nwg::Icon,
    pub minus: nwg::Icon,
    pub trash: nwg::Icon,
    pub folder: nwg::Icon,
    pub shield: nwg::Icon,
    pub rocket: nwg::Icon,
    pub clear: nwg::Icon,
    pub engine: nwg::Icon,
    pub pid: nwg::Icon,
    pub clock: nwg::Icon,
    pub ttl: nwg::Icon,
    pub flag: nwg::Icon,
    pub chart: nwg::Icon,
    pub globe: nwg::Icon,
    pub ok: nwg::Icon,
    pub bad: nwg::Icon,
    pub pending: nwg::Icon,
}

impl Glyphs {
    pub fn load() -> Glyphs {
        let s = px(16);
        let g = |b: &[u8]| icon_from_ico(b, s);
        Glyphs {
            status: g(glyph!("status")),
            flags: g(glyph!("flags")),
            log: g(glyph!("log")),
            settings: g(glyph!("settings")),
            power: g(glyph!("power")),
            check: g(glyph!("check")),
            tune: g(glyph!("tune")),
            help: g(glyph!("help")),
            apply: g(glyph!("apply")),
            undo: g(glyph!("undo")),
            plus: g(glyph!("plus")),
            minus: g(glyph!("minus")),
            trash: g(glyph!("trash")),
            folder: g(glyph!("folder")),
            shield: g(glyph!("shield")),
            rocket: g(glyph!("rocket")),
            clear: g(glyph!("clear")),
            engine: g(glyph!("engine")),
            pid: g(glyph!("pid")),
            clock: g(glyph!("clock")),
            ttl: g(glyph!("ttl")),
            flag: g(glyph!("flag")),
            chart: g(glyph!("chart")),
            globe: g(glyph!("globe")),
            ok: g(glyph!("ok")),
            bad: g(glyph!("bad")),
            pending: g(glyph!("pending")),
        }
    }
}

pub const APP_ICO: &[u8] = include_bytes!("../icons/app.ico");
pub const ON_ICO: &[u8] = include_bytes!("../icons/on.ico");
pub const OFF_ICO: &[u8] = include_bytes!("../icons/off.ico");
pub const BUSY_ICO: &[u8] = include_bytes!("../icons/busy.ico");

/// A 16 px glyph next to a piece of text.
pub fn glyph_frame<P: Into<ControlHandle>>(
    out: &mut nwg::ImageFrame,
    parent: P,
    icon: &nwg::Icon,
    pos: (i32, i32),
) -> Result<(), NwgError> {
    nwg::ImageFrame::builder()
        .icon(Some(icon))
        .position(pos)
        .size((16, 16))
        .parent(parent)
        .build(out)
}

/// Button with a leading glyph (comctl32 v6 draws icon + text).
pub fn icon_button<P: Into<ControlHandle>>(
    out: &mut nwg::Button,
    parent: P,
    text: &str,
    icon: &nwg::Icon,
    pos: (i32, i32),
    size: (i32, i32),
) -> Result<(), NwgError> {
    button(out, parent, &format!(" {text}"), pos, size)?;
    out.set_icon(Some(icon));
    Ok(())
}

/// Sets the label of an `icon_button`, keeping the gap after the glyph.
/// Returns true if it changed.
pub fn set_button_text(b: &nwg::Button, text: &str) -> bool {
    let t = format!(" {text}");
    if b.text() != t {
        b.set_text(&t);
        true
    } else {
        false
    }
}
