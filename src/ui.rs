//! Prism's chrome: a one-shot layered Win32 popup and a software-rendered
//! liquid-glass result list. No tray, no keyboard hook, no resident process.

use std::collections::HashSet;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::ptr::null_mut;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use ab_glyph::{point, Font, FontVec, PxScale, ScaleFont};

use crate::fuzzy::{case_flags, score_name, Query};
use crate::index::{self, Index};
use crate::refresh as incremental;
use crate::refresh::Outcome;
use crate::usn;
use crate::walk;
use crate::win32::*;

pub const WINDOW_CLASS: &str = "PrismFindWindow";
/// The window that a pre-launched, hidden Prism instance keeps alive.
///
/// A shell shortcut can only start a process; it cannot talk to a window. So the
/// shortcut starts `prism.exe --hotkey`, which finds a process already running
/// with this class in the same session and hands it the real window class to
/// show. Claiming a global hotkey is not this: it costs no keyboard hook, it never
/// watches what anyone types, and it shows nothing until the shell shortcut is
/// pressed. [`shutdown_placeholder`] posts [`WM_PLACEHOLDER_QUIT`] after showing,
/// which is why the process does not stay hidden for the rest of the session.
pub const PLACEHOLDER_CLASS: &str = "PrismHotkeyPlaceholder";
pub const WM_PLACEHOLDER_QUIT: u32 = WM_APP + 3;
const TIMER_ID: usize = 1;

const BASE_W: i32 = 820;
const BASE_H: i32 = 620;
const BASE_PAD: i32 = 20;
const OVERLAY_CAP: usize = 2048;
const SEARCH_DELAY: Duration = Duration::from_millis(140);
/// How long to wait after the last live (USN) change before re-running a query.
const OVERLAY_SETTLE: Duration = Duration::from_millis(120);

type Rgba = [u8; 4];

#[derive(Clone)]
struct Hit {
    name: String,
    parent: String,
    path: String,
    is_dir: bool,
}

struct Fonts {
    regular: FontVec,
    bold: Option<FontVec>,
}

impl Fonts {
    fn load() -> Option<Fonts> {
        let read = |names: &[&str]| -> Option<FontVec> {
            for n in names {
                if let Ok(bytes) = std::fs::read(n) {
                    if let Ok(f) = FontVec::try_from_vec(bytes) {
                        return Some(f);
                    }
                }
            }
            None
        };
        let regular = read(&[
            "C:\\Windows\\Fonts\\segoeui.ttf",
            "C:\\Windows\\Fonts\\arial.ttf",
            "C:\\Windows\\Fonts\\tahoma.ttf",
        ])?;
        let bold = read(&[
            "C:\\Windows\\Fonts\\segoeuib.ttf",
            "C:\\Windows\\Fonts\\arialbd.ttf",
            "C:\\Windows\\Fonts\\segoeuib.ttf",
        ]);
        Some(Fonts { regular, bold })
    }

    fn pick(&self, bold: bool) -> &FontVec {
        if bold {
            self.bold.as_ref().unwrap_or(&self.regular)
        } else {
            &self.regular
        }
    }
}

fn measure(font: &FontVec, text: &str, size: f32) -> f32 {
    let scale = PxScale { x: size, y: size };
    let sf = font.as_scaled(scale);
    text.chars().map(|c| sf.h_advance(sf.glyph_id(c))).sum()
}

fn ellipsize(font: &FontVec, text: &str, size: f32, max_w: f32) -> String {
    if max_w <= 0.0 || measure(font, text, size) <= max_w {
        return text.to_string();
    }
    let scale = PxScale { x: size, y: size };
    let sf = font.as_scaled(scale);
    let ell_w = sf.h_advance(sf.glyph_id('…'));
    let mut w = ell_w;
    let mut out = String::new();
    for c in text.chars() {
        let cw = sf.h_advance(sf.glyph_id(c));
        if w + cw > max_w {
            break;
        }
        w += cw;
        out.push(c);
    }
    out.push('…');
    out
}

struct Canvas {
    w: i32,
    h: i32,
    px: Vec<u8>,
}

impl Canvas {
    fn new(w: i32, h: i32) -> Canvas {
        Canvas {
            w,
            h,
            px: vec![0; (w.max(1) * h.max(1) * 4) as usize],
        }
    }

    #[inline]
    fn blend(&mut self, x: i32, y: i32, c: Rgba) {
        if x < 0 || y < 0 || x >= self.w || y >= self.h || c[3] == 0 {
            return;
        }
        let i = ((y * self.w + x) * 4) as usize;
        let sa = c[3] as u32;
        let ia = 255 - sa;
        let p = &mut self.px[i..i + 4];
        p[0] = ((c[0] as u32 * sa + p[0] as u32 * ia) / 255) as u8;
        p[1] = ((c[1] as u32 * sa + p[1] as u32 * ia) / 255) as u8;
        p[2] = ((c[2] as u32 * sa + p[2] as u32 * ia) / 255) as u8;
        p[3] = (sa + (p[3] as u32 * ia) / 255).min(255) as u8;
    }

    fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, c: Rgba) {
        for yy in y..(y + h).min(self.h) {
            for xx in x..(x + w).min(self.w) {
                self.blend(xx, yy, c);
            }
        }
    }

    fn fill_rrect(&mut self, x: i32, y: i32, w: i32, h: i32, r: f32, c: Rgba) {
        if w <= 0 || h <= 0 {
            return;
        }
        let x1 = x.max(0);
        let y1 = y.max(0);
        let x2 = (x + w).min(self.w);
        let y2 = (y + h).min(self.h);
        for yy in y1..y2 {
            for xx in x1..x2 {
                let d = rrect_sdf(
                    xx as f32 + 0.5,
                    yy as f32 + 0.5,
                    x as f32,
                    y as f32,
                    w as f32,
                    h as f32,
                    r,
                );
                let cov = (0.5 - d).clamp(0.0, 1.0);
                if cov > 0.0 {
                    let a = (c[3] as f32 * cov) as u8;
                    self.blend(xx, yy, [c[0], c[1], c[2], a]);
                }
            }
        }
    }

    fn outline_rrect(&mut self, x: i32, y: i32, w: i32, h: i32, r: f32, c: Rgba) {
        let x1 = (x - 2).max(0);
        let y1 = (y - 2).max(0);
        let x2 = (x + w + 2).min(self.w);
        let y2 = (y + h + 2).min(self.h);
        for yy in y1..y2 {
            for xx in x1..x2 {
                let d = rrect_sdf(
                    xx as f32 + 0.5,
                    yy as f32 + 0.5,
                    x as f32,
                    y as f32,
                    w as f32,
                    h as f32,
                    r,
                );
                if d.abs() <= 1.0 {
                    let a = (c[3] as f32 * (1.0 - d.abs())) as u8;
                    self.blend(xx, yy, [c[0], c[1], c[2], a]);
                }
            }
        }
    }

    fn fill_circle(&mut self, cx: f32, cy: f32, r: f32, c: Rgba) {
        let x1 = (cx - r - 2.0).max(0.0) as i32;
        let y1 = (cy - r - 2.0).max(0.0) as i32;
        let x2 = ((cx + r + 2.0) as i32).min(self.w);
        let y2 = ((cy + r + 2.0) as i32).min(self.h);
        for y in y1..y2 {
            for x in x1..x2 {
                let d = r - ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
                let cov = (d + 0.5).clamp(0.0, 1.0);
                if cov > 0.0 {
                    let a = (c[3] as f32 * cov) as u8;
                    self.blend(x, y, [c[0], c[1], c[2], a]);
                }
            }
        }
    }

    fn outline_circle(&mut self, cx: f32, cy: f32, r: f32, width: f32, c: Rgba) {
        let x1 = (cx - r - 2.0).max(0.0) as i32;
        let y1 = (cy - r - 2.0).max(0.0) as i32;
        let x2 = ((cx + r + 2.0) as i32).min(self.w);
        let y2 = ((cy + r + 2.0) as i32).min(self.h);
        for y in y1..y2 {
            for x in x1..x2 {
                let dist = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
                let d = (dist - r).abs();
                if d <= width / 2.0 + 0.5 {
                    let cov = ((width / 2.0 + 0.5) - d).clamp(0.0, 1.0);
                    let a = (c[3] as f32 * cov) as u8;
                    self.blend(x, y, [c[0], c[1], c[2], a]);
                }
            }
        }
    }

    fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, width: f32, c: Rgba) {
        let minx = (x0.min(x1) - width - 1.0).max(0.0) as i32;
        let miny = (y0.min(y1) - width - 1.0).max(0.0) as i32;
        let maxx = ((x0.max(x1) + width + 1.0) as i32).min(self.w);
        let maxy = ((y0.max(y1) + width + 1.0) as i32).min(self.h);
        for y in miny..maxy {
            for x in minx..maxx {
                let px = x as f32 + 0.5;
                let py = y as f32 + 0.5;
                let d = point_seg_dist(px, py, x0, y0, x1, y1);
                if d <= width / 2.0 + 0.5 {
                    let cov = ((width / 2.0 + 0.5) - d).clamp(0.0, 1.0);
                    let a = (c[3] as f32 * cov) as u8;
                    self.blend(x, y, [c[0], c[1], c[2], a]);
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn text(
        &mut self,
        fonts: &Fonts,
        text: &str,
        x: f32,
        y: f32,
        size: f32,
        color: Rgba,
        bold: bool,
        max_w: f32,
    ) -> f32 {
        let font = fonts.pick(bold);
        let shown = ellipsize(font, text, size, max_w);
        let scale = PxScale { x: size, y: size };
        let sf = font.as_scaled(scale);
        let mut pen = x;
        let baseline = y + sf.ascent();
        for ch in shown.chars() {
            let gid = sf.glyph_id(ch);
            let glyph = gid.with_scale_and_position(scale, point(pen, baseline));
            if let Some(outline) = font.outline_glyph(glyph) {
                let bb = outline.px_bounds();
                outline.draw(|gx, gy, cov| {
                    let px = (bb.min.x + gx as f32).round() as i32;
                    let py = (bb.min.y + gy as f32).round() as i32;
                    let a = (color[3] as f32 * cov) as u8;
                    self.blend(px, py, [color[0], color[1], color[2], a]);
                });
            }
            pen += sf.h_advance(gid);
        }
        pen
    }
}

fn rrect_sdf(px: f32, py: f32, x: f32, y: f32, w: f32, h: f32, r: f32) -> f32 {
    let cx = x + w / 2.0;
    let cy = y + h / 2.0;
    let hx = w / 2.0 - r;
    let hy = h / 2.0 - r;
    let qx = (px - cx).abs() - hx;
    let qy = (py - cy).abs() - hy;
    let ox = qx.max(0.0);
    let oy = qy.max(0.0);
    (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - r
}

fn point_seg_dist(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32) -> f32 {
    let dx = x1 - x0;
    let dy = y1 - y0;
    let len2 = dx * dx + dy * dy;
    if len2 <= 0.0001 {
        return ((px - x0).powi(2) + (py - y0).powi(2)).sqrt();
    }
    let t = (((px - x0) * dx + (py - y0) * dy) / len2).clamp(0.0, 1.0);
    let qx = x0 + t * dx;
    let qy = y0 + t * dy;
    ((px - qx).powi(2) + (py - qy).powi(2)).sqrt()
}

struct Ui {
    hwnd: HWND,
    hinstance: HINSTANCE,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    pad: i32,
    scale: f32,
    fonts: Fonts,

    index: Option<Index>,
    total_entries: usize,
    index_built_at: u32,
    query: String,
    caret: usize,
    sel_anchor: Option<usize>,
    field_selecting: bool,
    hovered: Option<usize>,
    tracking_leave: bool,
    results: Vec<Hit>,
    selected: usize,
    page: usize,
    page_size: usize,
    total_matches: usize,
    search_due: Option<Instant>,
    search_ms: f64,
    caret_on: bool,
    caret_epoch: u64,

    building: bool,
    building_refresh: bool,
    build_started: Instant,
    build_rx: Option<Receiver<BuildMsg>>,
    build_error: Option<String>,
    last_build_secs: u64,

    visible: bool,
    shown_at: Instant,
    activated_once: bool,
    quitting: bool,

    mem_dc: HDC,
    dib: HBITMAP,
    dib_old: HGDIOBJ,
    bits: *mut u8,
    surface_ok: bool,

    glass: Vec<u8>,
    row_rects: Vec<(usize, i32, i32, i32, i32)>,
    field_rect: (i32, i32, i32, i32),

    usn_rx: Option<Receiver<usn::Change>>,
    overlay: Vec<Hit>,
    overlay_seen: HashSet<String>,

    app_icon: HICON,
    app_icon_color: HBITMAP,
    app_icon_mask: HBITMAP,
}

struct BuildMsg {
    result: Result<Work, String>,
    out: PathBuf,
    final_path: PathBuf,
    cursors: Vec<(String, usn::Cursor)>,
}

/// What the background thread actually did.
enum Work {
    /// Index already matched the disk; nothing was written.
    Unchanged(usize, usize),
    /// Written by an incremental pass and ready to swap in.
    Refreshed(usize, usize),
    /// Full rebuild, written straight to the live path.
    Rebuilt(usize, usize),
}

impl Ui {
    fn new(hinstance: HINSTANCE, fonts: Fonts) -> Ui {
        let dpi = unsafe { GetDpiForSystem() };
        let scale = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 };
        let w = (BASE_W as f32 * scale) as i32;
        let h = (BASE_H as f32 * scale) as i32;
        let pad = (BASE_PAD as f32 * scale) as i32;
        Ui {
            hwnd: null_mut(),
            hinstance,
            x: 0,
            y: 0,
            w,
            h,
            pad,
            scale,
            fonts,
            index: None,
            total_entries: 0,
            index_built_at: 0,
            query: String::new(),
            caret: 0,
            sel_anchor: None,
            field_selecting: false,
            hovered: None,
            tracking_leave: false,
            results: Vec::new(),
            selected: 0,
            page: 0,
            page_size: 10,
            total_matches: 0,
            search_due: None,
            search_ms: 0.0,
            caret_on: true,
            caret_epoch: unsafe { GetTickCount64() },
            building: false,
            building_refresh: false,
            build_started: Instant::now(),
            build_rx: None,
            build_error: None,
            last_build_secs: 0,
            visible: false,
            shown_at: Instant::now(),
            activated_once: false,
            quitting: false,
            mem_dc: null_mut(),
            dib: null_mut(),
            dib_old: null_mut(),
            bits: null_mut(),
            surface_ok: false,
            glass: Vec::new(),
            row_rects: Vec::new(),
            field_rect: (0, 0, 0, 0),
            usn_rx: None,
            overlay: Vec::new(),
            overlay_seen: HashSet::new(),
            app_icon: null_mut(),
            app_icon_color: null_mut(),
            app_icon_mask: null_mut(),
        }
    }

    fn load_index(&mut self) -> bool {
        let path = index::default_index_path();
        match Index::open(&path) {
            Some(idx) => {
                idx.prefetch();
                self.total_entries = idx.count();
                self.index_built_at = idx.header.built_at;
                self.index = Some(idx);
                self.build_error = None;
                true
            }
            None => false,
        }
    }

    fn start_usn(&mut self) {
        if self.usn_rx.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.usn_rx = Some(rx);
        usn::start_tail(walk::default_roots(), self.index_built_at, tx);
    }

    /// Fault the sig/trigram arrays in on a throwaway thread, so the first query
    /// answers in ~30 ms instead of paging 95 MB while the caret blinks.
    fn warm_async(&mut self) {
        let path = index::default_index_path();
        std::thread::spawn(move || {
            unsafe {
                SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_LOWEST);
            }
            index::warm(&path);
        });
    }

    fn create_surface(&mut self) {
        unsafe {
            let screen = GetDC(null_mut());
            let mem = CreateCompatibleDC(screen);
            ReleaseDC(null_mut(), screen);
            if mem.is_null() {
                return;
            }
            let mut bmi = BITMAPINFO::default();
            bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bmi.bmiHeader.biWidth = self.w;
            bmi.bmiHeader.biHeight = -self.h;
            bmi.bmiHeader.biPlanes = 1;
            bmi.bmiHeader.biBitCount = 32;
            bmi.bmiHeader.biCompression = BI_RGB;
            let mut bits: *mut c_void = null_mut();
            let dib = CreateDIBSection(mem, &bmi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            if dib.is_null() || bits.is_null() {
                DeleteDC(mem);
                return;
            }
            let old = SelectObject(mem, dib);
            self.mem_dc = mem;
            self.dib = dib;
            self.dib_old = old;
            self.bits = bits as *mut u8;
            self.surface_ok = true;
        }
    }

    fn position(&mut self) {
        unsafe {
            let mut pt = POINT::default();
            GetCursorPos(&mut pt);
            let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            let ok = !mon.is_null() && GetMonitorInfoW(mon, &mut mi) != 0;
            let (left, top, right, bottom) = if ok {
                (
                    mi.rcWork.left,
                    mi.rcWork.top,
                    mi.rcWork.right,
                    mi.rcWork.bottom,
                )
            } else {
                (
                    0,
                    0,
                    GetSystemMetrics(0).max(self.w),
                    GetSystemMetrics(1).max(self.h),
                )
            };
            self.x = left + (right - left - self.w) / 2;
            self.y = top + ((bottom - top) as f32 * 0.14) as i32;
        }
    }

    fn show(&mut self) {
        self.position();
        if !self.surface_ok {
            self.create_surface();
        }
        // Capture before the window is visible, so the glass shows the desktop.
        self.capture_glass();
        self.caret_on = true;
        self.caret_epoch = unsafe { GetTickCount64() };
        unsafe {
            ShowWindow(self.hwnd, SW_SHOW);
            SetForegroundWindow(self.hwnd);
            SetFocus(self.hwnd);
            SetTimer(self.hwnd, TIMER_ID, 50, null_mut());
        }
        self.visible = true;
        self.shown_at = Instant::now();
        self.activated_once |= unsafe { GetForegroundWindow() == self.hwnd };
        self.render();
    }

    fn quit(&mut self) {
        self.quitting = true;
        unsafe {
            DestroyWindow(self.hwnd);
        }
    }

    fn start_build(&mut self) {
        self.start_work(false);
    }

    fn start_refresh(&mut self) {
        self.start_work(true);
    }

    fn start_work(&mut self, want_refresh: bool) {
        if self.building {
            return;
        }
        let roots = walk::default_roots();
        if roots.is_empty() {
            self.build_error = Some("No fixed drives found".into());
            self.render();
            return;
        }
        let refresh = want_refresh && self.index.is_some();
        let final_path = index::default_index_path();
        let out = if refresh {
            final_path.with_file_name("index.new.prsm")
        } else {
            final_path.clone()
        };
        let threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(8);
        // Snapshot only for a full build; the refresh engines persist their own
        // cursors (before the disk pass, and after the swap).
        // Only a full build snapshots here; the refresh engines hand back the
        // journal position their index covers, saved after a successful swap.
        let cursors = if refresh {
            Vec::new()
        } else {
            usn::snapshot(&roots)
        };
        let (tx, rx) = mpsc::channel();
        self.build_rx = Some(rx);
        self.building = true;
        self.building_refresh = refresh;
        self.build_started = Instant::now();
        self.last_build_secs = 0;
        self.build_error = None;
        std::thread::spawn(move || {
            unsafe {
                SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
            }
            let mut cursors = cursors;
            let result = if refresh {
                // Journal first: normally this reads a handful of records and
                // finds nothing to do. Only a rolled-over journal falls back to
                // the stat pass, and only a broken index to a full rebuild.
                match incremental::refresh_auto(&final_path, &out, &roots) {
                    Ok(done) => {
                        cursors = done.cursors;
                        match done.outcome {
                            Outcome::Refreshed(s) => Ok(Work::Refreshed(s.entries, s.dirs)),
                            Outcome::UpToDate(s) => Ok(Work::Unchanged(s.entries, s.dirs)),
                            Outcome::NeedsFullRebuild(_) => index::build(&roots, &out, threads)
                                .map(|s| Work::Rebuilt(s.entries, s.dirs))
                                .map_err(|e| e.to_string()),
                        }
                    }
                    Err(e) => Err(e),
                }
            } else {
                index::build(&roots, &out, threads)
                    .map(|s| Work::Rebuilt(s.entries, s.dirs))
                    .map_err(|e| e.to_string())
            };
            let _ = tx.send(BuildMsg {
                result,
                out,
                final_path,
                cursors,
            });
        });
    }

    fn poll_build(&mut self) -> bool {
        let Some(rx) = self.build_rx.take() else {
            return false;
        };
        match rx.try_recv() {
            Ok(msg) => {
                self.building = false;
                self.building_refresh = false;
                match msg.result {
                    Ok(work) => {
                        let (entries, swap) = match work {
                            // Nothing changed on disk: keep the live mapping, keep
                            // the file, do not touch the scratch path.
                            Work::Unchanged(entries, _) => (entries, false),
                            Work::Refreshed(entries, _) | Work::Rebuilt(entries, _) => {
                                (entries, true)
                            }
                        };
                        if swap {
                            // Drop the old mapping before replacing the file.
                            self.index = None;
                            if msg.out != msg.final_path {
                                let scratch_bak = msg.out.with_extension("prsm.bak");
                                if let Err(e) = std::fs::rename(&msg.out, &msg.final_path) {
                                    self.build_error = Some(format!("could not swap index: {e}"));
                                } else {
                                    // write_index backs up the scratch file too; drop
                                    // that copy now that the live index has been swapped.
                                    let _ = std::fs::remove_file(scratch_bak);
                                }
                            }
                            if !self.load_index() {
                                self.build_error =
                                    Some("index built but could not be opened".into());
                            } else if entries == 0 {
                                self.build_error = Some("index contains no entries".into());
                            } else if !msg.cursors.is_empty() {
                                // The index is live now, so the journal position it
                                // covers can be persisted.
                                usn::save_cursors(&msg.cursors);
                            }
                        }
                        // Whatever happened, never leave the UI without an index
                        // when a usable one exists on disk.
                        if self.index.is_none() && !self.load_index() {
                            self.build_error
                                .get_or_insert_with(|| "no usable index — rebuild required".into());
                        }
                        self.page = 0;
                        self.selected = 0;
                        self.hovered = None;
                        self.refresh();
                    }
                    Err(e) => {
                        self.build_error = Some(e);
                        // Keep the previous index usable if there was one.
                        if self.index.is_none() {
                            self.load_index();
                        }
                        self.refresh();
                    }
                }
                self.render();
                true
            }
            Err(TryRecvError::Empty) => {
                self.build_rx = Some(rx);
                false
            }
            Err(TryRecvError::Disconnected) => {
                self.building = false;
                self.build_error = Some("index builder stopped unexpectedly".into());
                self.render();
                true
            }
        }
    }

    fn refresh(&mut self) {
        self.search_due = None;
        self.page = 0;
        self.selected = 0;
        self.hovered = None;
        self.load_page(0);
    }

    fn load_page(&mut self, page: usize) {
        let q = self.query.trim().to_string();
        self.page = page;
        self.hovered = None;
        if q.is_empty() {
            self.results.clear();
            self.total_matches = 0;
            self.search_ms = 0.0;
            self.selected = 0;
            return;
        }

        let query = Query::parse(&q);
        let flags = case_flags(&q);
        let t = Instant::now();
        let page_size = self.page_size.max(1);
        let skip = page * page_size;

        // Fresh USN hits stay at the very top of the complete result list, so
        // every page starts with them and the index rows skip duplicates.
        let mut fresh: Vec<(i32, Hit)> = self
            .overlay
            .iter()
            .filter(|hit| Path::new(&hit.path).exists())
            .filter_map(|hit| {
                let (matched, score) = score_name(hit.name.as_bytes(), &query, &flags);
                (matched as usize == query.tokens.len()).then_some((score + 240, hit.clone()))
            })
            .collect();
        fresh.sort_unstable_by_key(|a| std::cmp::Reverse(a.0));
        let overlay_count = fresh.len();
        let end = skip + page_size + overlay_count + 16;

        let mut rows = Vec::with_capacity(end);
        let mut seen = HashSet::new();
        for (_, hit) in fresh {
            let key = hit.path.to_ascii_lowercase();
            if seen.insert(key) {
                rows.push(hit);
            }
        }

        let mut total = overlay_count;
        if let Some(idx) = self.index.as_ref() {
            let (hits, index_total) = idx.search_with_total(&query, &flags, end);
            total += index_total;
            for (i, _score, _tier) in hits {
                let i = i as usize;
                let path = idx.full_path(i);
                if !seen.insert(path.to_ascii_lowercase()) {
                    continue;
                }
                rows.push(Hit {
                    name: idx.name_str(i),
                    parent: idx.parent_path(i),
                    path,
                    is_dir: idx.is_dir(i),
                });
                if rows.len() >= skip + page_size {
                    break;
                }
            }
        }

        self.results = rows.into_iter().skip(skip).take(page_size).collect();
        self.total_matches = total;
        self.search_ms = t.elapsed().as_secs_f64() * 1000.0;
        self.selected = 0;
    }

    fn page_count(&self) -> usize {
        self.total_matches.div_ceil(self.page_size.max(1)).max(1)
    }

    fn select_page(&mut self, page: usize, select_last: bool) {
        let page = page.min(self.page_count() - 1);
        self.load_page(page);
        self.selected = if select_last {
            self.results.len().saturating_sub(1)
        } else {
            0
        };
        self.render();
    }

    fn push_overlay(&mut self, change: usn::Change) {
        let key = change.path.to_ascii_lowercase();
        if !self.overlay_seen.insert(key) {
            return;
        }
        if self.overlay.len() >= OVERLAY_CAP {
            let old = self.overlay.remove(0);
            self.overlay_seen.remove(&old.path.to_ascii_lowercase());
        }
        let name = change
            .path
            .rsplit('\\')
            .next()
            .unwrap_or(&change.path)
            .to_string();
        let parent = match change.path.rfind('\\') {
            Some(2) => change.path[..3].to_string(),
            Some(i) => change.path[..i].to_string(),
            None => String::new(),
        };
        self.overlay.push(Hit {
            name,
            parent,
            path: change.path,
            is_dir: change.is_dir,
        });
    }

    fn poll_usn(&mut self) -> bool {
        let Some(rx) = self.usn_rx.take() else {
            return false;
        };
        let mut changed = false;
        let mut disconnected = false;
        loop {
            match rx.try_recv() {
                Ok(change) => {
                    self.push_overlay(change);
                    changed = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        if !disconnected {
            self.usn_rx = Some(rx);
        }
        changed
    }

    fn set_query(&mut self, q: String) {
        self.query = q;
        self.caret = self.query.len();
        self.sel_anchor = None;
        self.query_changed();
    }

    fn query_changed(&mut self) {
        self.selected = 0;
        self.page = 0;
        self.hovered = None;
        self.reset_caret_blink();
        self.schedule_search();
        self.render();
    }

    fn schedule_search(&mut self) {
        if self.query.trim().is_empty() {
            self.search_due = None;
            self.results.clear();
            self.total_matches = 0;
            self.page = 0;
            self.search_ms = 0.0;
        } else {
            self.search_due = Some(Instant::now() + SEARCH_DELAY);
        }
    }

    fn flush_search(&mut self) {
        if self.search_due.take().is_some() {
            self.refresh();
        }
    }

    fn reset_caret_blink(&mut self) {
        self.caret_on = true;
        self.caret_epoch = unsafe { GetTickCount64() };
    }

    fn selection(&self) -> Option<(usize, usize)> {
        let anchor = self.sel_anchor?;
        (anchor != self.caret).then(|| (anchor.min(self.caret), anchor.max(self.caret)))
    }

    fn clamp_boundary(&self, pos: usize) -> usize {
        let mut pos = pos.min(self.query.len());
        while !self.query.is_char_boundary(pos) {
            pos -= 1;
        }
        pos
    }

    fn prev_boundary(&self, pos: usize) -> usize {
        self.query[..pos]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0)
    }

    fn next_boundary(&self, pos: usize) -> usize {
        self.query[pos..]
            .chars()
            .next()
            .map(|c| pos + c.len_utf8())
            .unwrap_or(self.query.len())
    }

    fn word_boundary(&self, delta: i32) -> usize {
        let mut pos = self.caret;
        if delta < 0 {
            while pos > 0 {
                let c = self.query[..pos].chars().next_back().unwrap();
                if !c.is_whitespace() {
                    break;
                }
                pos = self.prev_boundary(pos);
            }
            while pos > 0 {
                let c = self.query[..pos].chars().next_back().unwrap();
                if c.is_whitespace() {
                    break;
                }
                pos = self.prev_boundary(pos);
            }
        } else {
            while pos < self.query.len() {
                let c = self.query[pos..].chars().next().unwrap();
                if !c.is_whitespace() {
                    break;
                }
                pos = self.next_boundary(pos);
            }
            while pos < self.query.len() {
                let c = self.query[pos..].chars().next().unwrap();
                if c.is_whitespace() {
                    break;
                }
                pos = self.next_boundary(pos);
            }
        }
        pos
    }

    fn move_caret(&mut self, target: usize, extend: bool) {
        if extend {
            if self.sel_anchor.is_none() {
                self.sel_anchor = Some(self.caret);
            }
        } else {
            self.sel_anchor = None;
        }
        self.caret = self.clamp_boundary(target);
        if self.sel_anchor == Some(self.caret) {
            self.sel_anchor = None;
        }
        self.reset_caret_blink();
        self.render();
    }

    fn move_caret_by(&mut self, delta: i32, extend: bool, word: bool) {
        let target = if word {
            self.word_boundary(delta)
        } else if delta < 0 {
            self.prev_boundary(self.caret)
        } else {
            self.next_boundary(self.caret)
        };
        self.move_caret(target, extend);
    }

    fn select_all(&mut self) {
        self.sel_anchor = Some(0);
        self.caret = self.query.len();
        self.reset_caret_blink();
        self.render();
    }

    fn delete_selection(&mut self) -> bool {
        let Some((start, end)) = self.selection() else {
            return false;
        };
        self.query.replace_range(start..end, "");
        self.caret = start;
        self.sel_anchor = None;
        true
    }

    fn after_edit(&mut self) {
        self.caret = self.clamp_boundary(self.caret);
        self.sel_anchor = None;
        self.query_changed();
    }

    fn delete_back(&mut self, word: bool) {
        if self.delete_selection() {
            self.after_edit();
            return;
        }
        let start = if word {
            self.word_boundary(-1)
        } else {
            self.prev_boundary(self.caret)
        };
        if start == self.caret {
            return;
        }
        self.query.replace_range(start..self.caret, "");
        self.caret = start;
        self.after_edit();
    }

    fn delete_forward(&mut self, word: bool) {
        if self.delete_selection() {
            self.after_edit();
            return;
        }
        let end = if word {
            self.word_boundary(1)
        } else {
            self.next_boundary(self.caret)
        };
        if end == self.caret {
            return;
        }
        self.query.replace_range(self.caret..end, "");
        self.after_edit();
    }

    fn insert_text(&mut self, text: &str) {
        if !self.insert_raw(text) {
            // Pasted content: collapse its line breaks and runs of whitespace to
            // single spaces. A typed space is not pasted content and never comes
            // through here — see `insert_char`.
            let collapsed = text
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect::<String>()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            self.insert_raw(&collapsed);
        }
    }

    /// Put exactly `text` into the query, filling the selection. Returns `false`
    /// (and inserts nothing) when there is nothing left after trimming, which is
    /// how a clipboard holding one newline is rejected.
    fn insert_raw(&mut self, text: &str) -> bool {
        let clean = text.trim();
        if clean.is_empty() {
            return false;
        }

        let (start, end) = self.selection().unwrap_or((self.caret, self.caret));
        let mut q = String::with_capacity(self.query.len() + clean.len());
        q.push_str(&self.query[..start]);
        q.push_str(clean);
        q.push_str(&self.query[end..]);
        if q.chars().count() > 160 {
            let cut = q.char_indices().nth(160).map(|(i, _)| i).unwrap_or(q.len());
            q.truncate(cut);
        }
        self.caret = (start + clean.len()).min(q.len());
        self.query = q;
        self.after_edit();
        true
    }

    /// One typed character. Whitespace is kept exactly as typed: the space key
    /// arrives as `WM_CHAR` 0x20 just like any letter, and trimming it here is what
    /// made "prism agents" come out as "prismagents".
    fn insert_char(&mut self, c: char) {
        let (start, end) = self.selection().unwrap_or((self.caret, self.caret));
        let mut q = String::with_capacity(self.query.len() + c.len_utf8());
        q.push_str(&self.query[..start]);
        q.push(c);
        q.push_str(&self.query[end..]);
        if q.chars().count() > 160 {
            let cut = q.char_indices().nth(160).map(|(i, _)| i).unwrap_or(q.len());
            q.truncate(cut);
        }
        self.caret = (start + c.len_utf8()).min(q.len());
        self.query = q;
        self.after_edit();
    }

    fn copy_text_or_path(&mut self) {
        if let Some((start, end)) = self.selection() {
            write_clipboard(self.hwnd, &self.query[start..end]);
        } else {
            self.copy_selected();
        }
    }

    fn cut_text(&mut self) {
        let Some((start, end)) = self.selection() else {
            return;
        };
        write_clipboard(self.hwnd, &self.query[start..end]);
        self.delete_selection();
        self.after_edit();
    }

    fn paste_text(&mut self) {
        if let Some(text) = read_clipboard(self.hwnd) {
            self.insert_text(&text);
        }
    }

    fn on_char(&mut self, ch: u32) {
        let ctrl = unsafe { (GetKeyState(VK_CONTROL) as u16 & 0x8000) != 0 };
        match ch {
            0x08 if !ctrl => self.delete_back(false),
            0x7F => self.delete_forward(false),
            0x0D | 0x09 => {}
            // Space (0x20) is a character he types, not padding to be trimmed: the
            // search splits the query on whitespace, so a lost space silently turns
            // "prism agents" into one token.
            c if c >= 0x20 && c != 0x7F => {
                if let Some(ch) = char::from_u32(c) {
                    self.insert_char(ch);
                }
            }
            _ => {}
        }
    }

    fn on_key(&mut self, vk: i32) -> bool {
        let ctrl = unsafe { (GetKeyState(VK_CONTROL) as u16 & 0x8000) != 0 };
        let shift = unsafe { (GetKeyState(VK_SHIFT) as u16 & 0x8000) != 0 };
        if matches!(vk, VK_UP | VK_DOWN | VK_PRIOR | VK_NEXT | VK_RETURN) {
            self.flush_search();
        }
        match vk {
            VK_ESCAPE => {
                if self.query.is_empty() {
                    self.quit();
                } else {
                    self.set_query(String::new());
                }
            }
            VK_UP => self.move_selection(-1),
            VK_DOWN => self.move_selection(1),
            VK_PRIOR => self.select_page(self.page.saturating_sub(1), true),
            VK_NEXT => self.select_page(self.page + 1, false),
            VK_LEFT => self.move_caret_by(-1, shift, ctrl),
            VK_RIGHT => self.move_caret_by(1, shift, ctrl),
            VK_HOME => self.move_caret(0, shift),
            VK_END => self.move_caret(self.query.len(), shift),
            VK_RETURN => self.open_selected(ctrl),
            VK_BACK if ctrl => self.delete_back(true),
            VK_DELETE => self.delete_forward(ctrl),
            VK_A if ctrl => self.select_all(),
            VK_C if ctrl => self.copy_text_or_path(),
            VK_X if ctrl => self.cut_text(),
            VK_V if ctrl => self.paste_text(),
            _ => return false,
        }
        true
    }

    fn move_selection(&mut self, delta: i32) {
        if self.results.is_empty() {
            return;
        }
        let n = self.results.len() as i32;
        let next = self.selected as i32 + delta;
        if next < 0 {
            if self.page > 0 {
                self.select_page(self.page - 1, true);
            }
            return;
        }
        if next >= n {
            if self.page + 1 < self.page_count() {
                self.select_page(self.page + 1, false);
            } else {
                self.select_index(n as usize - 1);
            }
            return;
        }
        self.select_index(next as usize);
    }

    fn select_index(&mut self, i: usize) {
        if self.results.is_empty() {
            self.selected = 0;
        } else {
            self.selected = i.min(self.results.len() - 1);
        }
        self.render();
    }

    fn open_selected(&mut self, folder: bool) {
        if self.search_due.is_some() {
            self.refresh();
        }
        let Some(hit) = self.results.get(self.selected).cloned() else {
            return;
        };
        if folder {
            let params = format!("/select,\"{}\"", hit.path);
            shell_execute("explorer.exe", Some(&params));
        } else {
            shell_execute(&hit.path, None);
        }
        self.quit();
    }

    fn copy_selected(&mut self) {
        if let Some(hit) = self.results.get(self.selected) {
            write_clipboard(self.hwnd, &hit.path);
        }
    }

    fn row_at(&self, x: i32, y: i32) -> Option<usize> {
        self.row_rects
            .iter()
            .find(|r| x >= r.1 && x <= r.1 + r.3 && y >= r.2 && y <= r.2 + r.4)
            .map(|r| r.0)
    }

    fn set_pointer(&self, id: u16) {
        unsafe {
            SetCursor(LoadCursorW(null_mut(), id as *const u16));
        }
    }

    fn caret_from_x(&self, x: i32) -> usize {
        let text_x = self.field_rect.0 as f32 + 44.0 * self.scale;
        let size = 17.0 * self.scale;
        let sf = self.fonts.regular.as_scaled(PxScale { x: size, y: size });
        let mut pen = text_x;
        for (i, ch) in self.query.char_indices() {
            let w = sf.h_advance(sf.glyph_id(ch));
            if x as f32 <= pen + w / 2.0 {
                return i;
            }
            pen += w;
        }
        self.query.len()
    }

    fn select_word_at(&mut self, pos: usize) {
        let pos = self.clamp_boundary(pos);
        let mut start = pos;
        let mut end = pos;
        while start > 0 {
            let c = self.query[..start].chars().next_back().unwrap();
            if c.is_whitespace() {
                break;
            }
            start = self.prev_boundary(start);
        }
        while end < self.query.len() {
            let c = self.query[end..].chars().next().unwrap();
            if c.is_whitespace() {
                break;
            }
            end = self.next_boundary(end);
        }
        if start == end {
            self.select_all();
        } else {
            self.sel_anchor = Some(start);
            self.caret = end;
            self.reset_caret_blink();
            self.render();
        }
    }

    fn on_mouse_down(&mut self, lp: LPARAM, double: bool) {
        self.flush_search();
        let x = (lp & 0xFFFF) as u16 as i16 as i32;
        let y = ((lp >> 16) & 0xFFFF) as u16 as i16 as i32;

        if let Some(i) = self.row_at(x, y) {
            self.sel_anchor = None;
            self.selected = i;
            self.open_selected(unsafe { (GetKeyState(VK_CONTROL) as u16 & 0x8000) != 0 });
            return;
        }

        let (fx, fy, fw, fh) = self.field_rect;
        if x >= fx && x <= fx + fw && y >= fy && y <= fy + fh {
            unsafe {
                SetFocus(self.hwnd);
                SetCapture(self.hwnd);
            }
            self.field_selecting = true;
            let pos = self.caret_from_x(x);
            if double {
                self.select_word_at(pos);
                self.field_selecting = false;
                unsafe {
                    ReleaseCapture();
                }
            } else {
                self.caret = pos;
                self.sel_anchor = Some(pos);
                self.reset_caret_blink();
                self.render();
            }
        }
    }

    fn on_mouse_move(&mut self, lp: LPARAM, buttons: u32) {
        let x = (lp & 0xFFFF) as u16 as i16 as i32;
        let y = ((lp >> 16) & 0xFFFF) as u16 as i16 as i32;
        let row = self.row_at(x, y);
        let (fx, fy, fw, fh) = self.field_rect;
        let over_field = x >= fx && x <= fx + fw && y >= fy && y <= fy + fh;
        let mut changed = false;

        if row != self.hovered {
            self.hovered = row;
            changed = true;
        }
        if self.field_selecting && buttons & MK_LBUTTON != 0 {
            let pos = self.caret_from_x(x);
            if pos != self.caret {
                self.caret = pos;
                self.reset_caret_blink();
                changed = true;
            }
        }
        if changed {
            self.render();
        }
        if row.is_some() {
            self.set_pointer(IDC_HAND);
        } else if over_field {
            self.set_pointer(IDC_IBEAM);
        } else {
            self.set_pointer(IDC_ARROW);
        }

        if !self.tracking_leave {
            let mut track = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            unsafe {
                TrackMouseEvent(&mut track);
            }
            self.tracking_leave = true;
        }
    }

    fn on_mouse_up(&mut self) {
        if self.field_selecting {
            self.field_selecting = false;
            unsafe {
                ReleaseCapture();
            }
        }
    }

    fn on_mouse_leave(&mut self) {
        self.tracking_leave = false;
        if self.hovered.take().is_some() {
            self.render();
        }
        self.set_pointer(IDC_ARROW);
    }

    fn on_wheel(&mut self, wp: WPARAM) {
        let delta = ((wp >> 16) & 0xFFFF) as u16 as i16;
        if delta == 0 {
            return;
        }
        self.flush_search();
        if self.page_count() <= 1 {
            self.move_selection(if delta > 0 { -3 } else { 3 });
        } else if delta > 0 {
            self.select_page(self.page.saturating_sub(1), true);
        } else {
            self.select_page(self.page + 1, false);
        }
    }

    fn on_timer(&mut self) {
        let mut changed = self.poll_build();
        if self.poll_usn() && !self.query.trim().is_empty() {
            // A build touching thousands of files pushes a burst of overlay
            // entries. Re-running the search on every 50 ms tick would fight the
            // keyboard, so coalesce into one query a beat after the burst ends.
            let due = Instant::now() + OVERLAY_SETTLE;
            if !self.search_due.is_some_and(|d| d <= due) {
                self.search_due = Some(due);
            }
        }
        if self.search_due.is_some_and(|due| Instant::now() >= due) {
            self.refresh();
            changed = true;
        }
        let now = unsafe { GetTickCount64() };
        let caret = ((now - self.caret_epoch) / 550).is_multiple_of(2);
        if caret != self.caret_on {
            self.caret_on = caret;
            changed = true;
        }
        if self.building {
            let secs = self.build_started.elapsed().as_secs();
            if secs != self.last_build_secs {
                self.last_build_secs = secs;
                changed = true;
            }
        }
        if changed && self.visible {
            self.render();
        }
    }

    fn capture_glass(&mut self) {
        let w = self.w;
        let h = self.h;
        let mut glass = vec![0u8; (w * h * 4) as usize];
        let capture = capture_screen(self.x, self.y, w, h);
        let scale = self.scale;
        let pad = self.pad as f32;
        let inner_w = w as f32 - pad * 2.0;
        let inner_h = h as f32 - pad * 2.0;
        let radius = 18.0 * scale;

        let small = capture.as_ref().map(|c| blur_capture(c, w, h));
        for yy in 0..h {
            for xx in 0..w {
                let d = rrect_sdf(
                    xx as f32 + 0.5,
                    yy as f32 + 0.5,
                    pad,
                    pad,
                    inner_w,
                    inner_h,
                    radius,
                );
                let ga = (0.5 - d).clamp(0.0, 1.0) * 0.965;
                let sa = if d > 0.0 {
                    let t = (1.0 - d / pad).clamp(0.0, 1.0);
                    t * t * 0.52
                } else {
                    0.0
                };
                let a = ga + sa * (1.0 - ga);
                if a <= 0.002 {
                    continue;
                }
                let (mut br, mut bg, mut bb) = if let Some(s) = &small {
                    sample_blur(s, w, h, xx, yy)
                } else {
                    let t = yy as f32 / h as f32;
                    (
                        (22.0 * (1.0 - t) + 8.0 * t) as u8,
                        (26.0 * (1.0 - t) + 10.0 * t) as u8,
                        (38.0 * (1.0 - t) + 16.0 * t) as u8,
                    )
                };
                // Dark glass tint.
                br = (br as f32 * 0.36 + 11.0 * 0.64) as u8;
                bg = (bg as f32 * 0.36 + 13.0 * 0.64) as u8;
                bb = (bb as f32 * 0.36 + 20.0 * 0.64) as u8;
                // A faint top sheen.
                let rel = ((yy as f32 - pad) / inner_h).clamp(0.0, 1.0);
                let sheen = (1.0 - rel) * 7.0;
                br = (br as f32 + sheen).min(255.0) as u8;
                bg = (bg as f32 + sheen).min(255.0) as u8;
                bb = (bb as f32 + sheen * 1.15).min(255.0) as u8;

                let i = ((yy * w + xx) * 4) as usize;
                glass[i] = br;
                glass[i + 1] = bg;
                glass[i + 2] = bb;
                glass[i + 3] = (a * 255.0).clamp(0.0, 255.0) as u8;
            }
        }
        self.glass = glass;
    }

    fn render(&mut self) {
        if !self.visible || !self.surface_ok || self.glass.len() != (self.w * self.h * 4) as usize {
            return;
        }
        let w = self.w;
        let h = self.h;
        let s = self.scale;
        let pad = self.pad;
        let inner_x = pad;
        let inner_y = pad;
        let inner_w = w - pad * 2;
        let inner_h = h - pad * 2;
        let radius = 18.0 * s;

        let mut c = Canvas::new(w, h);
        c.px.copy_from_slice(&self.glass);

        // Outer hairline: the glass edge.
        c.outline_rrect(
            inner_x,
            inner_y,
            inner_w,
            inner_h,
            radius,
            [255, 255, 255, 34],
        );

        // Search field.
        let fx = inner_x + (16.0 * s) as i32;
        let fy = inner_y + (14.0 * s) as i32;
        let fw = inner_w - (32.0 * s) as i32;
        let fh = (50.0 * s) as i32;
        self.field_rect = (fx, fy, fw, fh);
        c.fill_rrect(fx, fy, fw, fh, 12.0 * s, [255, 255, 255, 16]);
        c.outline_rrect(fx, fy, fw, fh, 12.0 * s, [255, 255, 255, 26]);

        // Magnifier.
        let mx = fx as f32 + 24.0 * s;
        let my = fy as f32 + fh as f32 / 2.0 - 1.0 * s;
        c.outline_circle(mx, my, 6.0 * s, 1.6 * s, [214, 219, 227, 220]);
        c.line(
            mx + 4.6 * s,
            my + 4.6 * s,
            mx + 8.6 * s,
            my + 8.6 * s,
            1.8 * s,
            [214, 219, 227, 220],
        );

        let text_x = fx as f32 + 44.0 * s;
        let text_size = 17.0 * s;
        let text_max_w = fw as f32 - 60.0 * s;
        let line_y = fy as f32 + (fh as f32 - text_size) / 2.0 - 2.0 * s;
        let caret = self.clamp_boundary(self.caret);

        if let Some((start, end)) = self.selection() {
            let x1 = (text_x + measure(&self.fonts.regular, &self.query[..start], text_size))
                .clamp(text_x, text_x + text_max_w);
            let x2 = (text_x + measure(&self.fonts.regular, &self.query[..end], text_size))
                .clamp(text_x, text_x + text_max_w);
            if x2 > x1 {
                let sel_h = 22.0 * s;
                c.fill_rect(
                    x1.round() as i32,
                    (fy as f32 + (fh as f32 - sel_h) / 2.0) as i32,
                    (x2 - x1).round() as i32,
                    sel_h as i32,
                    [255, 255, 255, 50],
                );
            }
        }

        if self.query.is_empty() {
            let _ = c.text(
                &self.fonts,
                "Search every file on this PC",
                text_x,
                line_y,
                text_size,
                [150, 158, 172, 210],
                false,
                text_max_w,
            );
        } else {
            let _ = c.text(
                &self.fonts,
                &self.query,
                text_x,
                line_y,
                text_size,
                [244, 247, 252, 255],
                false,
                text_max_w,
            );
        }
        let caret_x = if self.query.is_empty() {
            text_x
        } else {
            (text_x + measure(&self.fonts.regular, &self.query[..caret], text_size))
                .min(text_x + text_max_w)
        };
        if self.caret_on {
            let caret_h = 22.0 * s;
            c.fill_rrect(
                caret_x.round() as i32,
                (fy as f32 + (fh as f32 - caret_h) / 2.0) as i32,
                (2.0 * s).max(1.0) as i32,
                caret_h as i32,
                1.0 * s,
                [238, 241, 246, 240],
            );
        }

        // Results / status.
        let rows_top = fy + fh + (12.0 * s) as i32;
        let footer_h = (34.0 * s) as i32;
        let rows_bottom = inner_y + inner_h - footer_h;
        let row_h = (45.0 * s) as i32;
        let max_rows = (((rows_bottom - rows_top) / row_h).max(1) as usize).min(10);
        self.page_size = max_rows;
        self.row_rects.clear();

        if self.index.is_none() {
            let title = if self.building_refresh {
                "Refreshing the index…"
            } else if self.building {
                "Building the index…"
            } else {
                "Index unavailable"
            };
            let cy = rows_top as f32 + (rows_bottom - rows_top) as f32 * 0.34;
            let tw = measure(self.fonts.pick(true), title, 22.0 * s);
            c.text(
                &self.fonts,
                title,
                (w as f32 - tw) / 2.0,
                cy,
                22.0 * s,
                [238, 243, 250, 255],
                true,
                w as f32,
            );
            let sub = if let Some(e) = &self.build_error {
                format!("{e} — run `prism --build-index`")
            } else if self.building_refresh {
                format!(
                    "Checking directory timestamps · {}s",
                    self.build_started.elapsed().as_secs()
                )
            } else if self.building {
                format!(
                    "One-time scan of every fixed drive · {}s",
                    self.build_started.elapsed().as_secs()
                )
            } else {
                "Run `prism --build-index` in a terminal".to_string()
            };
            let sw = measure(&self.fonts.regular, &sub, 13.5 * s);
            c.text(
                &self.fonts,
                &sub,
                (w as f32 - sw) / 2.0,
                cy + 34.0 * s,
                13.5 * s,
                [160, 170, 186, 235],
                false,
                w as f32 - 80.0 * s,
            );
        } else if self.query.trim().is_empty() {
            let title = "Search every file on this PC";
            let tw = measure(self.fonts.pick(true), title, 21.0 * s);
            let cy = rows_top as f32 + (rows_bottom - rows_top) as f32 * 0.34;
            c.text(
                &self.fonts,
                title,
                (w as f32 - tw) / 2.0,
                cy,
                21.0 * s,
                [238, 243, 250, 255],
                true,
                w as f32,
            );
            let sub = format!(
                "{} files and folders indexed · results appear as you type",
                format_count(self.total_entries)
            );
            let sw = measure(&self.fonts.regular, &sub, 13.5 * s);
            c.text(
                &self.fonts,
                &sub,
                (w as f32 - sw) / 2.0,
                cy + 32.0 * s,
                13.5 * s,
                [160, 170, 186, 235],
                false,
                w as f32 - 80.0 * s,
            );
        } else if self.results.is_empty() {
            let title = "No matches";
            let tw = measure(self.fonts.pick(true), title, 21.0 * s);
            let cy = rows_top as f32 + (rows_bottom - rows_top) as f32 * 0.34;
            c.text(
                &self.fonts,
                title,
                (w as f32 - tw) / 2.0,
                cy,
                21.0 * s,
                [238, 243, 250, 255],
                true,
                w as f32,
            );
            let sub = "Try fewer letters, or a different word";
            let sw = measure(&self.fonts.regular, sub, 13.5 * s);
            c.text(
                &self.fonts,
                sub,
                (w as f32 - sw) / 2.0,
                cy + 32.0 * s,
                13.5 * s,
                [160, 170, 186, 235],
                false,
                w as f32,
            );
        } else {
            for (row, hit) in self.results.iter().enumerate().take(max_rows) {
                let ry = rows_top + row as i32 * row_h;
                let rx = inner_x + (12.0 * s) as i32;
                let rw = inner_w - (24.0 * s) as i32;
                if row == self.selected {
                    c.fill_rrect(
                        rx,
                        ry,
                        rw,
                        row_h - (4.0 * s) as i32,
                        10.0 * s,
                        [255, 255, 255, 24],
                    );
                    c.fill_rrect(
                        rx + (6.0 * s) as i32,
                        ry + (10.0 * s) as i32,
                        (3.0 * s).max(1.0) as i32,
                        row_h - (24.0 * s) as i32,
                        2.0 * s,
                        [242, 245, 249, 190],
                    );
                } else if self.hovered == Some(row) {
                    c.fill_rrect(
                        rx,
                        ry,
                        rw,
                        row_h - (4.0 * s) as i32,
                        10.0 * s,
                        [255, 255, 255, 20],
                    );
                }
                self.row_rects
                    .push((row, rx, ry, rw, row_h - (4.0 * s) as i32));

                let icon_x = rx + (14.0 * s) as i32;
                let icon_y = ry + (row_h as f32 * 0.5 - 10.0 * s) as i32;
                draw_icon(&mut c, icon_x, icon_y, s, hit.is_dir);

                let tx = icon_x as f32 + 28.0 * s;
                let maxw = (rx + rw) as f32 - tx - 14.0 * s;
                c.text(
                    &self.fonts,
                    &hit.name,
                    tx,
                    ry as f32 + 6.0 * s,
                    15.0 * s,
                    [240, 244, 250, 255],
                    false,
                    maxw,
                );
                c.text(
                    &self.fonts,
                    &hit.parent,
                    tx,
                    ry as f32 + 24.0 * s,
                    11.5 * s,
                    [146, 156, 174, 235],
                    false,
                    maxw,
                );
            }
        }

        // Footer.
        let foot_y = inner_y + inner_h - footer_h;
        c.fill_rect(
            inner_x + (14.0 * s) as i32,
            foot_y,
            inner_w - (28.0 * s) as i32,
            (1.0 * s).max(1.0) as i32,
            [255, 255, 255, 18],
        );
        let hint = "↑↓ move   PgUp/PgDn page   ↵ open   ^↵ folder   ^C copy   esc";
        c.text(
            &self.fonts,
            hint,
            inner_x as f32 + 18.0 * s,
            foot_y as f32 + 11.0 * s,
            11.0 * s,
            [142, 152, 168, 225],
            false,
            inner_w as f32 - 330.0 * s,
        );
        let right = if self.building_refresh {
            format!("refreshing… {}s", self.build_started.elapsed().as_secs())
        } else if self.building {
            format!("indexing… {}s", self.build_started.elapsed().as_secs())
        } else if self.build_error.is_some() {
            "index error".to_string()
        } else if !self.query.trim().is_empty() {
            format!(
                "{} matches · {}/{} · {:.1} ms",
                format_count(self.total_matches),
                self.page + 1,
                self.page_count(),
                self.search_ms
            )
        } else {
            format!(
                "{} entries · {:.1} ms",
                format_count(self.total_entries),
                self.search_ms
            )
        };
        let rw = measure(&self.fonts.regular, &right, 11.0 * s);
        c.text(
            &self.fonts,
            &right,
            (inner_x + inner_w) as f32 - 18.0 * s - rw,
            foot_y as f32 + 11.0 * s,
            11.0 * s,
            [142, 152, 168, 225],
            false,
            inner_w as f32,
        );

        self.present(&c);
    }

    fn present(&mut self, c: &Canvas) {
        if self.bits.is_null() {
            return;
        }
        let n = (self.w * self.h * 4) as usize;
        let dst = unsafe { std::slice::from_raw_parts_mut(self.bits, n) };
        for i in (0..n).step_by(4) {
            let (r, g, b, a) = (c.px[i], c.px[i + 1], c.px[i + 2], c.px[i + 3]);
            let a32 = a as u32;
            dst[i] = ((b as u32 * a32) / 255) as u8;
            dst[i + 1] = ((g as u32 * a32) / 255) as u8;
            dst[i + 2] = ((r as u32 * a32) / 255) as u8;
            dst[i + 3] = a;
        }
        unsafe {
            let screen = GetDC(null_mut());
            let pt_dst = POINT {
                x: self.x,
                y: self.y,
            };
            let size = SIZE {
                cx: self.w,
                cy: self.h,
            };
            let src = POINT { x: 0, y: 0 };
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA,
            };
            UpdateLayeredWindow(
                self.hwnd,
                screen,
                &pt_dst,
                &size,
                self.mem_dc,
                &src,
                0,
                &blend,
                ULW_ALPHA,
            );
            ReleaseDC(null_mut(), screen);
        }
    }

    fn cleanup(&mut self) {
        unsafe {
            KillTimer(self.hwnd, TIMER_ID);
            if !self.mem_dc.is_null() {
                if !self.dib_old.is_null() {
                    SelectObject(self.mem_dc, self.dib_old);
                }
                if !self.dib.is_null() {
                    DeleteObject(self.dib);
                }
                DeleteDC(self.mem_dc);
                self.mem_dc = null_mut();
                self.dib = null_mut();
                self.bits = null_mut();
            }
            if !self.app_icon.is_null() {
                DestroyIcon(self.app_icon);
                self.app_icon = null_mut();
            }
            if !self.app_icon_color.is_null() {
                DeleteObject(self.app_icon_color);
                self.app_icon_color = null_mut();
            }
            if !self.app_icon_mask.is_null() {
                DeleteObject(self.app_icon_mask);
                self.app_icon_mask = null_mut();
            }
        }
    }
}

#[link(name = "user32")]
extern "system" {
    fn DestroyIcon(hIcon: HICON) -> BOOL;
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_NCCREATE {
            let cs = lp as *const CREATESTRUCTW;
            if !cs.is_null() {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, (*cs).lpCreateParams as isize);
            }
            return DefWindowProcW(hwnd, msg, wp, lp);
        }
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Ui;
        if ptr.is_null() {
            return DefWindowProcW(hwnd, msg, wp, lp);
        }
        let ui = &mut *ptr;
        match msg {
            WM_DESTROY => {
                ui.cleanup();
                PostQuitMessage(0);
                0
            }
            WM_CLOSE => {
                ui.quit();
                0
            }
            WM_ACTIVATE => {
                let inactive = (wp & 0xFFFF) as u32 == WA_INACTIVE;
                if !inactive {
                    ui.activated_once = true;
                } else if ui.activated_once
                    && ui.visible
                    && !ui.quitting
                    && ui.shown_at.elapsed().as_millis() > 180
                {
                    ui.quit();
                }
                0
            }
            WM_SHOW_PRISM => {
                if !ui.quitting {
                    ui.show();
                }
                0
            }
            WM_SETFOCUS => {
                // The window just came up (launch, or the shortcut from inside
                // another app). Select the query so the next keystroke starts a
                // fresh search instead of appending to the last one, and so an
                // arrow key still moves the caret.
                ui.select_all();
                0
            }
            WM_TIMER => {
                if wp == TIMER_ID {
                    ui.on_timer();
                }
                0
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                if ui.on_key(wp as i32) {
                    0
                } else {
                    DefWindowProcW(hwnd, msg, wp, lp)
                }
            }
            WM_CHAR => {
                ui.on_char(wp as u32);
                0
            }
            WM_LBUTTONDOWN => {
                ui.on_mouse_down(lp, false);
                0
            }
            WM_LBUTTONDBLCLK => {
                ui.on_mouse_down(lp, true);
                0
            }
            WM_LBUTTONUP => {
                ui.on_mouse_up();
                0
            }
            WM_MOUSEMOVE => {
                ui.on_mouse_move(lp, wp as u32);
                0
            }
            WM_MOUSELEAVE => {
                ui.on_mouse_leave();
                0
            }
            WM_MOUSEWHEEL => {
                ui.on_wheel(wp);
                0
            }
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                BeginPaint(hwnd, &mut ps);
                EndPaint(hwnd, &ps);
                0
            }
            WM_ERASEBKGND => 1,
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

fn format_count(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn draw_icon(c: &mut Canvas, x: i32, y: i32, s: f32, is_dir: bool) {
    if is_dir {
        let body = [154, 161, 173, 235];
        c.fill_rrect(
            x + (1.0 * s) as i32,
            y + (7.0 * s) as i32,
            (16.0 * s) as i32,
            (10.0 * s) as i32,
            2.0 * s,
            body,
        );
        c.fill_rrect(
            x + (1.0 * s) as i32,
            y + (4.0 * s) as i32,
            (8.0 * s) as i32,
            (6.0 * s) as i32,
            2.0 * s,
            body,
        );
    } else {
        let paper = [224, 229, 238, 235];
        c.fill_rrect(
            x + (3.0 * s) as i32,
            y + (1.0 * s) as i32,
            (12.0 * s) as i32,
            (16.0 * s) as i32,
            2.0 * s,
            paper,
        );
        c.fill_rect(
            x + (6.0 * s) as i32,
            y + (6.0 * s) as i32,
            (6.0 * s) as i32,
            (1.0 * s).max(1.0) as i32,
            [120, 128, 142, 230],
        );
        c.fill_rect(
            x + (6.0 * s) as i32,
            y + (9.0 * s) as i32,
            (6.0 * s) as i32,
            (1.0 * s).max(1.0) as i32,
            [120, 128, 142, 230],
        );
        c.fill_rect(
            x + (6.0 * s) as i32,
            y + (12.0 * s) as i32,
            (4.0 * s) as i32,
            (1.0 * s).max(1.0) as i32,
            [120, 128, 142, 230],
        );
    }
}

fn shell_execute(file: &str, params: Option<&str>) {
    let op = wide("open");
    let f = wide(file);
    let p = params.map(wide);
    unsafe {
        ShellExecuteW(
            null_mut(),
            op.as_ptr(),
            f.as_ptr(),
            p.as_ref().map(|v| v.as_ptr()).unwrap_or(std::ptr::null()),
            std::ptr::null(),
            SW_SHOWNORMAL,
        );
    }
}

fn read_clipboard(hwnd: HWND) -> Option<String> {
    unsafe {
        if OpenClipboard(hwnd) == 0 {
            return None;
        }
        let h = GetClipboardData(CF_UNICODETEXT);
        if h.is_null() {
            CloseClipboard();
            return None;
        }
        let p = GlobalLock(h) as *const u16;
        if p.is_null() {
            CloseClipboard();
            return None;
        }
        let mut len = 0usize;
        while *p.add(len) != 0 {
            len += 1;
        }
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
        GlobalUnlock(h);
        CloseClipboard();
        Some(s)
    }
}

fn write_clipboard(hwnd: HWND, text: &str) {
    unsafe {
        if OpenClipboard(hwnd) == 0 {
            return;
        }
        EmptyClipboard();
        let w = wide(text);
        let h = GlobalAlloc(GMEM_MOVEABLE, w.len() * 2);
        if !h.is_null() {
            let dst = GlobalLock(h) as *mut u16;
            if !dst.is_null() {
                std::ptr::copy_nonoverlapping(w.as_ptr(), dst, w.len());
                GlobalUnlock(h);
                if SetClipboardData(CF_UNICODETEXT, h).is_null() {
                    GlobalFree(h);
                }
            } else {
                GlobalFree(h);
            }
        }
        CloseClipboard();
    }
}

fn capture_screen(x: i32, y: i32, w: i32, h: i32) -> Option<Vec<u8>> {
    unsafe {
        let screen = GetDC(null_mut());
        if screen.is_null() {
            return None;
        }
        let mem = CreateCompatibleDC(screen);
        if mem.is_null() {
            ReleaseDC(null_mut(), screen);
            return None;
        }
        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = w;
        bmi.bmiHeader.biHeight = -h;
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB;
        let mut bits: *mut c_void = null_mut();
        let dib = CreateDIBSection(mem, &bmi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
        if dib.is_null() || bits.is_null() {
            DeleteDC(mem);
            ReleaseDC(null_mut(), screen);
            return None;
        }
        let old = SelectObject(mem, dib);
        let ok = BitBlt(mem, 0, 0, w, h, screen, x, y, SRCCOPY) != 0;
        let out = if ok {
            let src = std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize);
            Some(src.to_vec())
        } else {
            None
        };
        SelectObject(mem, old);
        DeleteObject(dib);
        DeleteDC(mem);
        ReleaseDC(null_mut(), screen);
        out
    }
}

fn blur_capture(src: &[u8], w: i32, h: i32) -> Vec<u8> {
    // Downscale by 8 (BRGX → RGB), a couple of box passes, then bilinear upscale.
    let f = 8i32;
    let sw = (w / f).max(1);
    let sh = (h / f).max(1);
    let mut small = vec![0u8; (sw * sh * 3) as usize];
    for sy in 0..sh {
        for sx in 0..sw {
            let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
            for oy in 0..f {
                let yy = sy * f + oy;
                if yy >= h {
                    break;
                }
                for ox in 0..f {
                    let xx = sx * f + ox;
                    if xx >= w {
                        break;
                    }
                    let i = ((yy * w + xx) * 4) as usize;
                    b += src[i] as u32;
                    g += src[i + 1] as u32;
                    r += src[i + 2] as u32;
                    n += 1;
                }
            }
            let n = n.max(1);
            let i = ((sy * sw + sx) * 3) as usize;
            small[i] = (r / n) as u8;
            small[i + 1] = (g / n) as u8;
            small[i + 2] = (b / n) as u8;
        }
    }
    for _ in 0..2 {
        box_blur_rgb(&mut small, sw, sh, 2);
    }
    small
}

fn box_blur_rgb(buf: &mut [u8], w: i32, h: i32, r: i32) {
    let src = buf.to_vec();
    for y in 0..h {
        for x in 0..w {
            let (mut ar, mut ag, mut ab, mut n) = (0u32, 0u32, 0u32, 0u32);
            for dy in -r..=r {
                let yy = (y + dy).clamp(0, h - 1);
                for dx in -r..=r {
                    let xx = (x + dx).clamp(0, w - 1);
                    let i = ((yy * w + xx) * 3) as usize;
                    ar += src[i] as u32;
                    ag += src[i + 1] as u32;
                    ab += src[i + 2] as u32;
                    n += 1;
                }
            }
            let i = ((y * w + x) * 3) as usize;
            buf[i] = (ar / n) as u8;
            buf[i + 1] = (ag / n) as u8;
            buf[i + 2] = (ab / n) as u8;
        }
    }
}

fn sample_blur(small: &[u8], w: i32, h: i32, x: i32, y: i32) -> (u8, u8, u8) {
    let f = 8.0f32;
    let sw = (w / 8).max(1);
    let sh = (h / 8).max(1);
    let fx = (x as f32 / f - 0.5).clamp(0.0, (sw - 1) as f32);
    let fy = (y as f32 / f - 0.5).clamp(0.0, (sh - 1) as f32);
    let x0 = fx.floor() as i32;
    let y0 = fy.floor() as i32;
    let x1 = (x0 + 1).min(sw - 1);
    let y1 = (y0 + 1).min(sh - 1);
    let tx = fx - x0 as f32;
    let ty = fy - y0 as f32;
    let at = |xx: i32, yy: i32, c: usize| small[((yy * sw + xx) * 3) as usize + c] as f32;
    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
    let mut out = [0u8; 3];
    for (c, slot) in out.iter_mut().enumerate() {
        let top = lerp(at(x0, y0, c), at(x1, y0, c), tx);
        let bot = lerp(at(x0, y1, c), at(x1, y1, c), tx);
        *slot = lerp(top, bot, ty) as u8;
    }
    (out[0], out[1], out[2])
}

fn icon_over(dst: &mut [f32; 4], color: Rgba, coverage: f32) {
    let sa = (color[3] as f32 / 255.0) * coverage.clamp(0.0, 1.0);
    if sa <= 0.0 {
        return;
    }
    let da = dst[3] / 255.0;
    let oa = sa + da * (1.0 - sa);
    if oa <= 0.0 {
        return;
    }
    for i in 0..3 {
        dst[i] = (color[i] as f32 * sa + dst[i] * da * (1.0 - sa)) / oa;
    }
    dst[3] = oa * 255.0;
}

fn make_app_icon() -> (HICON, HBITMAP, HBITMAP) {
    unsafe {
        let size = 32i32;
        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = size;
        bmi.bmiHeader.biHeight = -size;
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB;
        let mut bits: *mut c_void = null_mut();
        let color = CreateDIBSection(null_mut(), &bmi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
        if color.is_null() || bits.is_null() {
            return (null_mut(), null_mut(), null_mut());
        }
        let px = bits as *mut u8;
        let mut mask_bits = vec![0u8; (size * size / 8) as usize];
        for y in 0..size {
            for x in 0..size {
                let fx = x as f32 + 0.5;
                let fy = y as f32 + 0.5;
                let mut rgba = [0.0f32; 4];

                // Black rounded tile; the hairline keeps it legible on dark bars.
                let tile = rrect_sdf(fx, fy, 2.0, 2.0, 28.0, 28.0, 7.0);
                let cov = (0.5 - tile).clamp(0.0, 1.0);
                let t = (fy / 32.0).clamp(0.0, 1.0);
                icon_over(
                    &mut rgba,
                    [
                        (15.0 - 10.0 * t) as u8,
                        (16.0 - 11.0 * t) as u8,
                        (20.0 - 13.0 * t) as u8,
                        255,
                    ],
                    cov,
                );
                let edge = (1.1 - tile.abs()).clamp(0.0, 1.0);
                icon_over(&mut rgba, [255, 255, 255, 52], edge);

                // White magnifier: a thin ring and a round-capped handle.
                let dx = fx - 15.5;
                let dy = fy - 14.5;
                let dist = (dx * dx + dy * dy).sqrt();
                let ring = (1.15 - (dist - 6.0).abs()).clamp(0.0, 1.0);
                icon_over(&mut rgba, [250, 251, 253, 255], ring);
                let handle = point_seg_dist(fx, fy, 19.8, 18.8, 24.2, 23.2);
                let hcov = (1.6 - handle).clamp(0.0, 1.0);
                icon_over(&mut rgba, [250, 251, 253, 255], hcov);

                let i = ((y * size + x) * 4) as usize;
                px.add(i).write(rgba[2] as u8);
                px.add(i + 1).write(rgba[1] as u8);
                px.add(i + 2).write(rgba[0] as u8);
                px.add(i + 3).write(rgba[3] as u8);
                if rgba[3] < 0.5 {
                    let mi = y * size + x;
                    mask_bits[(mi / 8) as usize] |= 1 << (mi % 8);
                }
            }
        }
        let mask = CreateBitmap(size, size, 1, 1, mask_bits.as_ptr() as *const c_void);
        let ii = ICONINFO {
            fIcon: 1,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon = CreateIconIndirect(&ii);
        (icon, color, mask)
    }
}

pub fn run() {
    unsafe {
        SetProcessDpiAwarenessContext(-4);
        let hinstance = GetModuleHandleW(std::ptr::null());
        let mutex_name = wide("Local\\PrismFindWindow");
        let _mutex = CreateMutexW(std::ptr::null(), 0, mutex_name.as_ptr());
        if !_mutex.is_null() && GetLastError() == ERROR_ALREADY_EXISTS {
            let class = wide(WINDOW_CLASS);
            let hwnd = FindWindowW(class.as_ptr(), std::ptr::null());
            if !hwnd.is_null() {
                PostMessageW(hwnd, WM_SHOW_PRISM, 0, 0);
            }
            return;
        }

        let Some(fonts) = Fonts::load() else {
            let text = wide("Prism could not load a system font.");
            let title = wide("Prism");
            MessageBoxW(
                null_mut(),
                text.as_ptr(),
                title.as_ptr(),
                MB_ICONERROR | MB_OK,
            );
            return;
        };

        let mut ui = Box::new(Ui::new(hinstance, fonts));
        let (icon, color, mask) = make_app_icon();
        ui.app_icon = icon;
        ui.app_icon_color = color;
        ui.app_icon_mask = mask;

        let class = wide(WINDOW_CLASS);
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            hCursor: LoadCursorW(null_mut(), 32512 as *const u16),
            lpszClassName: class.as_ptr(),
            hIcon: if icon.is_null() {
                LoadIconW(null_mut(), 32512 as *const u16)
            } else {
                icon
            },
            hIconSm: if icon.is_null() {
                LoadIconW(null_mut(), 32512 as *const u16)
            } else {
                icon
            },
            ..Default::default()
        };
        if RegisterClassExW(&wc) == 0 {
            let text = wide("Prism could not register its window class.");
            let title = wide("Prism");
            MessageBoxW(
                null_mut(),
                text.as_ptr(),
                title.as_ptr(),
                MB_ICONERROR | MB_OK,
            );
            return;
        }

        let ptr = (&mut *ui) as *mut Ui as *mut c_void;
        let title = wide("Prism");
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            class.as_ptr(),
            title.as_ptr(),
            WS_POPUP,
            0,
            0,
            100,
            100,
            null_mut(),
            null_mut(),
            hinstance,
            ptr,
        );
        if hwnd.is_null() {
            return;
        }
        ui.hwnd = hwnd;
        ui.create_surface();

        if ui.load_index() {
            // Journal-first freshness check, on every launch: it reads a few
            // thousand bytes and usually proves the index is current, in which
            // case nothing is rewritten. Only a real backlog costs work.
            ui.start_refresh();
            ui.warm_async();
        } else {
            ui.start_build();
        }
        ui.start_usn();
        ui.show();

        // Ctrl+Shift+Space is Prism's shortcut, system-wide, and it only waits:
        // the process is posted to and shown, never left running, so the "no
        // resident agent" rule still holds. It is not taken by anything he uses
        // (not Win+Space for layout, not Ctrl+Space in Sta recognisers, not
        // Ctrl+Shift+V, not Ctrl+Shift+T/N/E/P, not Ctrl+Alt+Space in
        // PowerShell) and it is the same shape as the Ctrl+Space local find in
        // an editor or a terminal — one modifier up in scope.
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

// ---------------------------------------------------------------------------
// Instant-open lane
//
// There is no global hotkey to claim. Windows will not hand a permanent chord to
// a program that is allowed to be closed, and a low-level keyboard hook costs more
// than the feature is worth: it watches every keystroke on the machine, it is the
// exact shape of a keylogger, it needs a process alive from boot, and it would
// break the one rule this app is built on. So the shortcut lives where shortcuts
// actually live — a `.lnk` on the desktop and pinned to the taskbar — and Prism
// is made fast enough that pressing it is not a wait. Nothing here runs while
// Prism is closed.
//
// `prism.exe --hotkey` is that shortcut's target. If a Prism window already
// exists it drains that instance's message queue, which is what an external
// `PostMessage` can do without walking into its renderer, and exits: one small
// extra process for a few hundredths of a second, no window to flash. If none
// exists it is an ordinary launch, because a shortcut has to work from cold too.
// ---------------------------------------------------------------------------

/// File name of the shortcut Prism installs: `Prism.lnk` on the desktop.
pub const SHORTCUT_NAME: &str = "Prism";

/// Command-line flag the shortcut is pointed at.
pub const SHORTCUT_ARG: &str = "--hotkey";
