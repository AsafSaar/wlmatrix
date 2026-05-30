// wlmatrix — native Wayland Matrix digital-rain screensaver.
//
// Renders pixels on the CPU into a wl_shm buffer (softbuffer) — deliberately NO
// OpenGL/GPU, because the GL/GLX path is broken on this box (every xscreensaver
// hack and GL window failed to fullscreen here). winit gives us a real Wayland
// fullscreen surface. Any keypress / mouse movement / click exits, so it
// dismisses instantly. The wl-screensaver daemon launches it on idle.
//
// Configuration: wlmatrix/config.toml under the OS config dir (flat key = value,
// TOML-compatible), overridable by CLI flags. See HELP and default_config_path().
//
// Portability: the renderer (winit + softbuffer + ab_glyph) is cross-platform —
// it builds and runs on Linux (Wayland/X11), macOS and Windows. Fonts are found
// via the OS font database (fontdb) and the config dir via `dirs`, so there are
// no hardcoded paths. The idle-launch *screensaver* glue (dist/) is Linux-only
// for now; on macOS/Windows wlmatrix runs as a normal fullscreen app.

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use ab_glyph::{Font, FontVec, Glyph, Point, ScaleFont};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Fullscreen, Window, WindowId};

// Monospace font *family names* we prefer, in order. We query these against the
// OS font database (fontdb) rather than hardcoding file paths, so this works on
// Linux, macOS and Windows. The list spans all three: DejaVu/Liberation/Noto
// (Linux), Menlo/SF Mono/Monaco (macOS), Consolas/Cascadia/Courier New (Windows).
const MONO_FAMILIES: &[&str] = &[
    "DejaVu Sans Mono", "Liberation Mono", "Noto Sans Mono", "Ubuntu Mono",
    "Menlo", "SF Mono", "Monaco", "Consolas", "Cascadia Mono", "Courier New",
];
// CJK-capable families for non-latin charsets (e.g. katakana), again by name and
// spanning all three platforms.
const CJK_FAMILIES: &[&str] = &[
    "Noto Sans Mono CJK JP", "Noto Sans CJK JP", "Source Han Sans",
    "Hiragino Sans", "Hiragino Kaku Gothic ProN", // macOS
    "Yu Gothic", "Meiryo", "MS Gothic", "Microsoft YaHei", // Windows
];
// half-width katakana — single-width, fits the rain grid (full-width would overlap)
const KATAKANA: std::ops::RangeInclusive<char> = '\u{FF66}'..='\u{FF9D}';
const DEFAULT_CHARSET: &str =
    "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789@#$%&*+-/<>=?!:";

// ============================== configuration ==============================

#[derive(Clone)]
struct Config {
    font_px: f32,
    fps: u64,
    speed_min: f32,
    speed_max: f32,
    head: (u8, u8, u8),
    trail: (u8, u8, u8),
    charset: String,
    font_path: Option<String>,
    glow: bool,    // additive bloom on bright pixels ("neo" look)
    depth: usize,  // number of parallax rain layers (1 = flat)
    // "operator view": a faint image/text hidden in the rain (see paint_ghost).
    mask: Option<String>,      // path to a grayscale/any PNG used as a luminance mask
    mask_text: Option<String>, // text rendered into a mask (used if `mask` is unset)
    mask_intensity: f32,       // 0..1, how strongly the ghost glows under the rain
    mask_contrast: f32,        // 0..1, how much the rain OUTSIDE the figure is dimmed
}

impl Default for Config {
    fn default() -> Self {
        Config {
            font_px: 26.0,
            fps: 30,
            speed_min: 6.0,
            speed_max: 24.0,
            head: (220, 255, 220),
            trail: (0, 255, 40),
            charset: DEFAULT_CHARSET.to_string(),
            font_path: None,
            glow: false,
            depth: 1,
            mask: None,
            mask_text: None,
            // 0.85: visible against the heavier "neo" look (bloom + depth + dense
            // katakana all raise the brightness floor); still reads as a ghost.
            mask_intensity: 0.85,
            mask_contrast: 0.0, // opt-in; 0 = pure additive ghost (no negative-space dimming)
        }
    }
}

fn parse_bool(s: &str) -> bool {
    matches!(s.trim().to_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

/// The per-OS config file: `wlmatrix/config.toml` under the platform config dir
/// — `~/.config` (Linux, honoring `$XDG_CONFIG_HOME`), `~/Library/Application
/// Support` (macOS), or `%APPDATA%` (Windows). Resolved by the `dirs` crate.
fn default_config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("wlmatrix/config.toml"))
}

/// Parse a flat `key = value` file (a subset of TOML: scalars + quoted strings,
/// `#` comments, `[section]` headers ignored) and fold it into `cfg`.
fn parse_config_text(text: &str, cfg: &mut Config) {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let key = k.trim();
        let v = v.trim();
        let val = if let Some(rest) = v.strip_prefix('"') {
            // quoted string: take up to the closing quote (keeps '#' inside)
            rest.split('"').next().unwrap_or(rest).to_string()
        } else {
            // bare scalar: strip any inline comment
            v.split('#').next().unwrap_or(v).trim().to_string()
        };
        apply_key(cfg, key, &val);
    }
}

/// Apply one setting (from the file or a CLI flag) onto `cfg`.
fn apply_key(cfg: &mut Config, key: &str, val: &str) {
    match key {
        "fps" => {
            if let Ok(v) = val.parse::<u64>() {
                cfg.fps = v.clamp(1, 240);
            }
        }
        "font_size" | "font-size" => {
            if let Ok(v) = val.parse::<f32>() {
                if v >= 4.0 {
                    cfg.font_px = v;
                }
            }
        }
        "speed_min" => {
            if let Ok(v) = val.parse() {
                cfg.speed_min = v;
            }
        }
        "speed_max" => {
            if let Ok(v) = val.parse() {
                cfg.speed_max = v;
            }
        }
        "speed" => {
            // "min-max" (e.g. 4-20) or a single number (interpreted as the max)
            if let Some((a, b)) = val.split_once('-') {
                if let (Ok(a), Ok(b)) = (a.trim().parse(), b.trim().parse()) {
                    cfg.speed_min = a;
                    cfg.speed_max = b;
                }
            } else if let Ok(v) = val.parse::<f32>() {
                cfg.speed_min = v * 0.4;
                cfg.speed_max = v;
            }
        }
        "color" => {
            if let Some((h, t)) = resolve_color(val) {
                cfg.head = h;
                cfg.trail = t;
            }
        }
        "charset" => cfg.charset = resolve_charset(val),
        "font" => cfg.font_path = Some(val.to_string()),
        "glow" => cfg.glow = parse_bool(val),
        "depth" => {
            if let Ok(v) = val.parse::<usize>() {
                cfg.depth = v.clamp(1, 4);
            }
        }
        "style" => match val.trim().to_lowercase().as_str() {
            // "neo" = the modern look: bloom + katakana. Explicit charset/glow
            // keys placed after `style` in the file (or CLI flags) still win.
            "neo" => {
                cfg.glow = true;
                cfg.charset = KATAKANA.collect();
            }
            "classic" | "matrix" => cfg.glow = false,
            _ => {}
        },
        "mask" => cfg.mask = if val.is_empty() { None } else { Some(val.to_string()) },
        "mask_text" | "mask-text" => {
            cfg.mask_text = if val.is_empty() { None } else { Some(val.to_string()) }
        }
        "mask_intensity" | "mask-intensity" => {
            if let Ok(v) = val.parse::<f32>() {
                cfg.mask_intensity = v.clamp(0.0, 1.0);
            }
        }
        "mask_contrast" | "mask-contrast" => {
            if let Ok(v) = val.parse::<f32>() {
                cfg.mask_contrast = v.clamp(0.0, 1.0);
            }
        }
        "idle_ms" => {} // consumed by the wl-screensaver daemon, not the renderer
        _ => {}
    }
    if cfg.speed_min > cfg.speed_max {
        std::mem::swap(&mut cfg.speed_min, &mut cfg.speed_max);
    }
    cfg.speed_min = cfg.speed_min.max(0.5);
}

/// Map a color name or `#RRGGBB` to (head, trail) RGB. Head is the bright leader,
/// trail is the color the falling tail fades through.
fn resolve_color(s: &str) -> Option<((u8, u8, u8), (u8, u8, u8))> {
    Some(match s.trim().to_lowercase().as_str() {
        "green" => ((220, 255, 220), (0, 255, 40)),
        "amber" | "orange" => ((255, 240, 200), (255, 176, 0)),
        "cyan" | "blue" => ((210, 255, 255), (0, 255, 210)),
        "white" => ((255, 255, 255), (170, 170, 170)),
        "red" | "crimson" => ((255, 210, 210), (255, 40, 40)),
        "purple" | "magenta" => ((245, 210, 255), (200, 0, 255)),
        hex => {
            let h = hex.trim_start_matches('#');
            if h.len() != 6 {
                return None;
            }
            let r = u8::from_str_radix(&h[0..2], 16).ok()?;
            let g = u8::from_str_radix(&h[2..4], 16).ok()?;
            let b = u8::from_str_radix(&h[4..6], 16).ok()?;
            // head = the trail color brightened halfway toward white
            let head = (
                ((r as u16 + 255) / 2) as u8,
                ((g as u16 + 255) / 2) as u8,
                ((b as u16 + 255) / 2) as u8,
            );
            (head, (r, g, b))
        }
    })
}

/// Resolve a charset preset name to its characters, or treat the value as a
/// literal set of characters. Empty falls back to the default.
fn resolve_charset(s: &str) -> String {
    let out = match s.to_lowercase().as_str() {
        "ascii" => DEFAULT_CHARSET.to_string(),
        "alnum" => "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789".to_string(),
        "binary" => "01".to_string(),
        "digits" => "0123456789".to_string(),
        // half-width katakana — needs a CJK-capable font (auto-detected)
        "katakana" => KATAKANA.collect(),
        _ => s.to_string(),
    };
    if out.chars().next().is_none() {
        DEFAULT_CHARSET.to_string()
    } else {
        out
    }
}

// ============================== rendering core ==============================

// ---- tiny xorshift RNG (avoids a rand crate dependency) ----
struct Rng(u64);
impl Rng {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: u32) -> u32 {
        (self.next_u64() % n.max(1) as u64) as u32
    }
    fn frac(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }
}

// Pre-rasterized glyph coverage (alpha 0..255) + placement within a cell.
struct GlyphBmp {
    w: usize,
    h: usize,
    left: i32,
    top: i32,
    cov: Vec<u8>,
}

/// A luminance field (0..1, row-major) used as the hidden "operator view" image.
/// Sourced from a PNG (decode_png_luminance) or rendered text (text_to_mask).
struct MaskData {
    lum: Vec<f32>,
    w: usize,
    h: usize,
}

struct Column {
    head: f32,
    speed: f32, // rows per second
    len: i32,
    chars: Vec<u8>, // index into the glyph set for each row
}

type Surf = softbuffer::Surface<Rc<Window>, Rc<Window>>;

/// One depth plane of rain. With depth > 1, far layers are smaller, slower and
/// dimmer than near ones; the planes are composited back-to-front (additively).
struct Layer {
    glyphs: Vec<GlyphBmp>, // glyph cache at this layer's size
    cell_w: usize,
    cell_h: usize,
    cols: usize,
    rows: usize,
    columns: Vec<Column>,
    speed_scale: f32, // multiplies the base fall speed
    brightness: f32,  // 0..1, dims far layers
}

struct App {
    window: Option<Rc<Window>>,
    surface: Option<Surf>,
    layers: Vec<Layer>, // back (far) to front (near)
    rng: Rng,
    last: Instant,
    next_frame: Instant,
    inited_grid: bool,
    start: Instant,
    // from Config:
    fps: u64,
    speed_min: f32,
    speed_max: f32,
    head: (u8, u8, u8),
    trail: (u8, u8, u8),
    glow: bool,
    // hidden "operator view" image (None = plain rain):
    mask: Option<MaskData>,
    mask_intensity: f32,
    mask_contrast: f32,
    ghost_weights: Vec<f32>, // per-cell mask luminance on the NEAR layer's grid
    ghost_idx: Vec<u8>,      // per-cell glyph index for the ghost (shimmer over time)
    ghost_tick: u32,
}

/// Rasterize the charset's glyphs at `font_px`; returns (glyphs, cell_w, cell_h).
fn build_glyphs(font: &FontVec, charset: &str, font_px: f32) -> (Vec<GlyphBmp>, usize, usize) {
    let scaled = font.as_scaled(font_px);
    let ascent = scaled.ascent();
    // Size the cell to the widest glyph so proportional (CJK) fonts don't overlap.
    let cell_w = charset
        .chars()
        .map(|c| scaled.h_advance(font.glyph_id(c)))
        .fold(0.0_f32, f32::max)
        .ceil()
        .max(1.0) as usize;
    let cell_h = (scaled.ascent() - scaled.descent()).ceil().max(1.0) as usize;

    let mut glyphs = Vec::new();
    for ch in charset.chars() {
        let glyph: Glyph = font
            .glyph_id(ch)
            .with_scale_and_position(font_px, Point { x: 0.0, y: 0.0 });
        if let Some(outline) = font.outline_glyph(glyph) {
            let b = outline.px_bounds();
            let gw = b.width().ceil() as usize;
            let gh = b.height().ceil() as usize;
            let mut cov = vec![0u8; gw * gh];
            outline.draw(|x, y, c| {
                let (xi, yi) = (x as usize, y as usize);
                if xi < gw && yi < gh {
                    cov[yi * gw + xi] = (c * 255.0) as u8;
                }
            });
            glyphs.push(GlyphBmp {
                w: gw,
                h: gh,
                left: b.min.x.round() as i32,
                top: (ascent + b.min.y).round() as i32,
                cov,
            });
        } else {
            glyphs.push(GlyphBmp { w: 0, h: 0, left: 0, top: 0, cov: vec![] });
        }
    }
    if glyphs.is_empty() {
        glyphs.push(GlyphBmp { w: 0, h: 0, left: 0, top: 0, cov: vec![] });
    }
    (glyphs, cell_w, cell_h)
}

/// Load a font that can render `charset`. Resolution order: an explicit
/// `font_path`, then preferred families queried against the OS font database
/// (CJK families first for non-latin charsets), then the OS's generic monospace,
/// then any monospaced face that covers the charset. Cross-platform via fontdb —
/// no hardcoded file paths. Returns (font, covers_charset).
fn load_font(charset: &str, font_path: &Option<String>) -> (FontVec, bool) {
    let needs_cjk = charset.chars().any(|c| c as u32 > 0x2FF);
    let sample = charset.chars().next().unwrap_or('M');

    // 1) explicit override path (e.g. config `font = "..."`).
    if let Some(p) = font_path {
        if let Ok(data) = std::fs::read(p) {
            if let Ok(font) = FontVec::try_from_vec_and_index(data, 0) {
                let covered = font.glyph_id(sample).0 != 0;
                return (font, covered);
            }
        }
        eprintln!("wlmatrix: font '{p}' not usable; falling back to a system font");
    }

    // 2) ask the OS for fonts (works on Linux, macOS and Windows).
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    // ab_glyph FontVec from a fontdb face id (handles .ttc face indices).
    let load = |db: &fontdb::Database, id: fontdb::ID| -> Option<FontVec> {
        db.with_face_data(id, |data, index| FontVec::try_from_vec_and_index(data.to_vec(), index).ok())
            .flatten()
    };
    let covers = |f: &FontVec| f.glyph_id(sample).0 != 0;

    // Preferred families by name (CJK first when the charset needs it).
    let mut families: Vec<&str> = Vec::new();
    if needs_cjk {
        families.extend(CJK_FAMILIES);
    }
    families.extend(MONO_FAMILIES);

    let mut fallback: Option<FontVec> = None;
    for name in families {
        let q = fontdb::Query { families: &[fontdb::Family::Name(name)], ..Default::default() };
        if let Some(font) = db.query(&q).and_then(|id| load(&db, id)) {
            if covers(&font) {
                return (font, true);
            }
            fallback.get_or_insert(font);
        }
    }
    // 3) the OS's generic monospace family.
    let q = fontdb::Query { families: &[fontdb::Family::Monospace], ..Default::default() };
    if let Some(font) = db.query(&q).and_then(|id| load(&db, id)) {
        if covers(&font) {
            return (font, true);
        }
        fallback.get_or_insert(font);
    }
    // 4) last resort: any monospaced face that actually covers the sample
    //    (catches CJK on systems whose family names we didn't anticipate).
    for face in db.faces() {
        if !face.monospaced {
            continue;
        }
        if let Some(font) = load(&db, face.id) {
            if covers(&font) {
                return (font, true);
            }
            fallback.get_or_insert(font);
        }
    }

    (fallback.expect("no usable font found on this system"), false)
}

/// Decode any PNG into a luminance mask (0..1). Palette/low-bit-depth images are
/// expanded and 16-bit is stripped to 8-bit, so we always see 1–4 u8 samples per
/// pixel. Luminance = Rec.601 weights, premultiplied by alpha (transparent → 0).
fn decode_png_luminance(path: &str) -> Option<MaskData> {
    let file = std::fs::File::open(path).ok()?;
    let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width as usize, info.height as usize);
    let samples = info.color_type.samples(); // 1=gray 2=gray+a 3=rgb 4=rgba
    let data = &buf[..info.buffer_size()];
    let lum601 = |r: u8, g: u8, b: u8| 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
    let mut lum = vec![0f32; w * h];
    for (i, out) in lum.iter_mut().enumerate() {
        let p = i * samples;
        let (l, a) = match samples {
            1 => (data[p] as f32, 255.0),
            2 => (data[p] as f32, data[p + 1] as f32),
            3 => (lum601(data[p], data[p + 1], data[p + 2]), 255.0),
            _ => (lum601(data[p], data[p + 1], data[p + 2]), data[p + 3] as f32),
        };
        *out = (l / 255.0) * (a / 255.0);
    }
    Some(MaskData { lum, w, h })
}

/// Rasterize a line of text into a luminance mask (white-on-black) at ~120px,
/// so it can be hidden in the rain. Uses glyph coverage as luminance.
fn text_to_mask(text: &str, font: &FontVec) -> Option<MaskData> {
    let px = 120.0_f32;
    let scaled = font.as_scaled(px);
    let ascent = scaled.ascent();
    let height = (scaled.ascent() - scaled.descent()).ceil().max(1.0) as usize;
    let total_w: f32 = text.chars().map(|c| scaled.h_advance(font.glyph_id(c))).sum();
    let pad = 8usize;
    let w = total_w.ceil().max(1.0) as usize + pad * 2;
    let h = height + pad * 2;
    let mut lum = vec![0f32; w * h];
    let mut pen_x = pad as f32;
    for ch in text.chars() {
        let gid = font.glyph_id(ch);
        let glyph = gid.with_scale_and_position(px, Point { x: pen_x, y: 0.0 });
        if let Some(outline) = font.outline_glyph(glyph) {
            let b = outline.px_bounds();
            let base_x = b.min.x.round() as i32;
            let base_y = (ascent + b.min.y).round() as i32 + pad as i32;
            outline.draw(|gx, gy, c| {
                let xx = base_x + gx as i32;
                let yy = base_y + gy as i32;
                if xx >= 0 && (xx as usize) < w && yy >= 0 && (yy as usize) < h {
                    let idx = yy as usize * w + xx as usize;
                    if c > lum[idx] {
                        lum[idx] = c; // max coverage (glyphs shouldn't overlap, but be safe)
                    }
                }
            });
        }
        pen_x += scaled.h_advance(gid);
    }
    Some(MaskData { lum, w, h })
}

/// Resolve the configured hidden image: a PNG path takes precedence, else text.
/// `font` is the already-loaded rain font (reused so text masks need no 2nd load).
fn load_mask(cfg: &Config, font: &FontVec) -> Option<MaskData> {
    if let Some(p) = &cfg.mask {
        match decode_png_luminance(p) {
            Some(m) => return Some(m),
            None => eprintln!("wlmatrix: could not read mask PNG '{p}' — ignoring"),
        }
    }
    match &cfg.mask_text {
        Some(t) if !t.is_empty() => text_to_mask(t, font),
        _ => None,
    }
}

/// Map a mask (any size) onto a `cols`×`rows` glyph grid, fit-contain & centered,
/// sampling luminance at each cell's center. Cells outside the image read 0.
fn compute_ghost(mask: &MaskData, cols: usize, rows: usize, cell_w: usize, cell_h: usize) -> Vec<f32> {
    let mut out = vec![0f32; cols * rows];
    if mask.w == 0 || mask.h == 0 || cols == 0 || rows == 0 {
        return out;
    }
    let screen_w = (cols * cell_w) as f32;
    let screen_h = (rows * cell_h) as f32;
    // fit-contain: scale the image to fit inside the screen, preserving aspect
    let scale = (screen_w / mask.w as f32).min(screen_h / mask.h as f32);
    let off_x = (screen_w - mask.w as f32 * scale) * 0.5;
    let off_y = (screen_h - mask.h as f32 * scale) * 0.5;
    for cy in 0..rows {
        let my = ((cy as f32 + 0.5) * cell_h as f32 - off_y) / scale;
        if my < 0.0 || my >= mask.h as f32 {
            continue;
        }
        for cx in 0..cols {
            let mx = ((cx as f32 + 0.5) * cell_w as f32 - off_x) / scale;
            if mx < 0.0 || mx >= mask.w as f32 {
                continue;
            }
            out[cy * cols + cx] = mask.lum[my as usize * mask.w + mx as usize];
        }
    }
    out
}

impl App {
    fn new(cfg: &Config) -> App {
        let needs_cjk = cfg.charset.chars().any(|c| c as u32 > 0x2FF);
        let (font, covered) = load_font(&cfg.charset, &cfg.font_path);
        let charset = if needs_cjk && !covered {
            eprintln!("wlmatrix: no font found covering this charset; falling back to ASCII");
            DEFAULT_CHARSET.to_string()
        } else {
            cfg.charset.clone()
        };
        let depth = cfg.depth.clamp(1, 4);
        let mut layers = Vec::with_capacity(depth);
        for j in 0..depth {
            // j = 0 is farthest, depth-1 is nearest; t in (0, 1].
            let t = (j + 1) as f32 / depth as f32;
            let (size, speed_scale, brightness) = if depth == 1 {
                (1.0, 1.0, 1.0)
            } else {
                (0.5 + 0.5 * t, 0.45 + 0.55 * t, 0.4 + 0.6 * t)
            };
            let (glyphs, cell_w, cell_h) = build_glyphs(&font, &charset, cfg.font_px * size);
            layers.push(Layer {
                glyphs,
                cell_w,
                cell_h,
                cols: 0,
                rows: 0,
                columns: Vec::new(),
                speed_scale,
                brightness,
            });
        }

        // Build the hidden-image mask now, while the rain `font` is still in scope
        // (text masks reuse it). `ghost_*` grids are sized in rebuild_grid.
        let mask = load_mask(cfg, &font);

        App {
            window: None,
            surface: None,
            layers,
            rng: Rng(0x9E3779B97F4A7C15),
            last: Instant::now(),
            next_frame: Instant::now(),
            inited_grid: false,
            start: Instant::now(),
            fps: cfg.fps,
            speed_min: cfg.speed_min,
            speed_max: cfg.speed_max,
            head: cfg.head,
            trail: cfg.trail,
            glow: cfg.glow,
            mask,
            mask_intensity: cfg.mask_intensity,
            mask_contrast: cfg.mask_contrast,
            ghost_weights: Vec::new(),
            ghost_idx: Vec::new(),
            ghost_tick: 0,
        }
    }

    fn rebuild_grid(&mut self, width: usize, height: usize) {
        let (smin, smax) = (self.speed_min, self.speed_max);
        for li in 0..self.layers.len() {
            let cell_w = self.layers[li].cell_w;
            let cell_h = self.layers[li].cell_h;
            let speed_scale = self.layers[li].speed_scale;
            let nglyphs = self.layers[li].glyphs.len() as u32;
            let cols = (width / cell_w).max(1);
            let rows = (height / cell_h).max(1);
            let mut columns = Vec::with_capacity(cols);
            for _ in 0..cols {
                let mut chars = vec![0u8; rows];
                for c in chars.iter_mut() {
                    *c = self.rng.below(nglyphs) as u8;
                }
                let speed = (smin + self.rng.frac() * (smax - smin)) * speed_scale;
                let len = 6 + self.rng.below((rows as u32 / 2).max(7)) as i32;
                let head = -(self.rng.below(rows as u32) as f32);
                columns.push(Column { head, speed, len, chars });
            }
            let layer = &mut self.layers[li];
            layer.cols = cols;
            layer.rows = rows;
            layer.columns = columns;
        }
        // Size the ghost to the NEAR (front) layer's grid. Extract its scalars
        // into a value before touching self.rng (disjoint-borrow dance).
        let ghost = self.mask.as_ref().and_then(|mask| {
            self.layers.last().map(|near| {
                (
                    compute_ghost(mask, near.cols, near.rows, near.cell_w, near.cell_h),
                    near.glyphs.len() as u32,
                )
            })
        });
        if let Some((weights, nglyphs)) = ghost {
            let mut idx = vec![0u8; weights.len()];
            for v in idx.iter_mut() {
                *v = self.rng.below(nglyphs) as u8;
            }
            self.ghost_weights = weights;
            self.ghost_idx = idx;
        }
        self.inited_grid = true;
    }

    fn step(&mut self, dt: f32) {
        let (smin, smax) = (self.speed_min, self.speed_max);
        for li in 0..self.layers.len() {
            let rows = self.layers[li].rows as i32;
            let nglyphs = self.layers[li].glyphs.len() as u32;
            let speed_scale = self.layers[li].speed_scale;
            for ci in 0..self.layers[li].columns.len() {
                let (old_i, new_i) = {
                    let col = &mut self.layers[li].columns[ci];
                    let old_i = col.head.floor() as i32;
                    col.head += col.speed * dt;
                    (old_i, col.head.floor() as i32)
                };
                for y in (old_i + 1)..=new_i {
                    if y >= 0 && y < rows {
                        let c = (self.rng.next_u64() % nglyphs as u64) as u8;
                        self.layers[li].columns[ci].chars[y as usize] = c;
                    }
                }
                if new_i - self.layers[li].columns[ci].len > rows {
                    let speed = (smin + self.rng.frac() * (smax - smin)) * speed_scale;
                    let len = 6 + self.rng.below((rows as u32 / 2).max(7)) as i32;
                    let head = -(self.rng.below(rows as u32 + 1) as f32) - len as f32;
                    let col = &mut self.layers[li].columns[ci];
                    col.head = head;
                    col.speed = speed;
                    col.len = len;
                }
            }
        }
        // Subtle shimmer: every ~6 frames, re-roll ~5% of the ghost glyphs so the
        // hidden image flickers like the rest of the rain rather than sitting still.
        if !self.ghost_idx.is_empty() {
            self.ghost_tick = self.ghost_tick.wrapping_add(1);
            if self.ghost_tick % 6 == 0 {
                let nglyphs = self.layers.last().map(|l| l.glyphs.len() as u32).unwrap_or(1);
                let n = (self.ghost_idx.len() / 20).max(1);
                for _ in 0..n {
                    let pos = self.rng.below(self.ghost_idx.len() as u32) as usize;
                    self.ghost_idx[pos] = self.rng.below(nglyphs) as u8;
                }
            }
        }
    }

    fn render(&mut self) {
        let (Some(window), Some(surface)) = (self.window.as_ref(), self.surface.as_mut()) else {
            return;
        };
        let size = window.inner_size();
        let (width, height) = (size.width as usize, size.height as usize);
        if width == 0 || height == 0 {
            return;
        }
        if surface
            .resize(
                NonZeroU32::new(size.width).unwrap(),
                NonZeroU32::new(size.height).unwrap(),
            )
            .is_err()
        {
            return;
        }
        let mut buf = match surface.buffer_mut() {
            Ok(b) => b,
            Err(_) => return,
        };
        render_layers(
            &mut buf,
            width,
            height,
            &self.layers,
            self.head,
            self.trail,
            self.glow,
            &self.ghost_weights,
            &self.ghost_idx,
            self.mask_intensity,
            self.mask_contrast,
        );
        let _ = buf.present();
    }
}

/// Per-cell rain attenuation derived from the mask's negative space — the
/// `mask_contrast` knob. Rain glyphs outside the figure are dimmed so the
/// silhouette reads as figure-ground, not just brighter glyphs. Weights live on
/// the near-layer grid; we sample by pixel position so it applies uniformly to
/// every depth layer regardless of that layer's own cell size.
struct Atten<'a> {
    weights: &'a [f32],
    cols: usize,
    rows: usize,
    cell_w: usize,
    cell_h: usize,
    contrast: f32,
}
impl Atten<'_> {
    /// Brightness multiplier for a point at pixel (px, py): 1.0 inside the figure
    /// (weight → 1), down to `1 - contrast` in empty space (weight → 0).
    fn mult_at(&self, px: i32, py: i32) -> f32 {
        let cx = (px.max(0) as usize / self.cell_w.max(1)).min(self.cols.saturating_sub(1));
        let cy = (py.max(0) as usize / self.cell_h.max(1)).min(self.rows.saturating_sub(1));
        let w = self.weights[cy * self.cols + cx];
        1.0 - self.contrast * (1.0 - w)
    }
}

/// Composite all depth layers (far → near, additively) into `buf`, then bloom.
/// If a ghost mask is present it is laid down FIRST (under the rain) so rain
/// heads crossing bright ghost cells brighten them — the image shimmers through.
/// With `mask_contrast > 0`, the rain outside the figure is additionally dimmed.
#[allow(clippy::too_many_arguments)]
fn render_layers(
    buf: &mut [u32],
    width: usize,
    height: usize,
    layers: &[Layer],
    head: (u8, u8, u8),
    trail: (u8, u8, u8),
    glow: bool,
    ghost_weights: &[f32],
    ghost_idx: &[u8],
    mask_intensity: f32,
    mask_contrast: f32,
) {
    for px in buf.iter_mut() {
        *px = 0; // clear once; layers add on top
    }
    // The near (front) layer owns the ghost grid; both the ghost and the
    // negative-space attenuation are keyed off it.
    let near = layers.last();
    // Negative-space dimming (opt-in): only when a mask is present AND contrast > 0.
    let atten = if mask_contrast > 0.0 && !ghost_weights.is_empty() {
        near.map(|n| Atten {
            weights: ghost_weights,
            cols: n.cols,
            rows: n.rows,
            cell_w: n.cell_w,
            cell_h: n.cell_h,
            contrast: mask_contrast,
        })
    } else {
        None
    };
    // Hidden image, drawn under the rain using the near layer's glyph cache/grid.
    if !ghost_weights.is_empty() {
        if let Some(n) = near {
            paint_ghost(
                buf,
                width,
                height,
                &n.glyphs,
                ghost_weights,
                ghost_idx,
                n.cell_w,
                n.cell_h,
                n.cols,
                n.rows,
                trail,
                mask_intensity,
            );
        }
    }
    let dim = |c: (u8, u8, u8), b: f32| {
        ((c.0 as f32 * b) as u8, (c.1 as f32 * b) as u8, (c.2 as f32 * b) as u8)
    };
    for layer in layers {
        paint_frame(
            buf,
            width,
            height,
            &layer.glyphs,
            &layer.columns,
            layer.cell_w,
            layer.cell_h,
            layer.rows,
            dim(head, layer.brightness),
            dim(trail, layer.brightness),
            atten.as_ref(),
        );
    }
    if glow {
        apply_bloom(buf, width, height);
    }
}

/// Draw one frame of rain into `buf` (pixels are 0x00RRGGBB). Shared by the live
/// renderer, `--shot` (PNG) and `--gif`.
#[allow(clippy::too_many_arguments)]
fn paint_frame(
    buf: &mut [u32],
    width: usize,
    height: usize,
    glyphs: &[GlyphBmp],
    columns: &[Column],
    cell_w: usize,
    cell_h: usize,
    rows: usize,
    head: (u8, u8, u8),
    trail: (u8, u8, u8),
    atten: Option<&Atten>,
) {
    // additive: caller clears once, then each depth layer adds on top
    let cw = cell_w as i32;
    let chh = cell_h as i32;
    for (cx, col) in columns.iter().enumerate() {
        let head_i = col.head.floor() as i32;
        let cell_x = cx as i32 * cw;
        for k in 0..=col.len {
            let y = head_i - k;
            if y < 0 || y >= rows as i32 {
                continue;
            }
            let ch_idx = col.chars[y as usize] as usize;
            let g = &glyphs[ch_idx];
            if g.cov.is_empty() {
                continue;
            }
            // leader uses the head color; the trail fades through the trail color
            let (mut r, mut gr, mut b) = if k == 0 {
                (head.0 as f32, head.1 as f32, head.2 as f32)
            } else {
                let t = 1.0 - (k as f32 / col.len as f32);
                let inten = (0.25 + 0.75 * t).min(1.0);
                (trail.0 as f32 * inten, trail.1 as f32 * inten, trail.2 as f32 * inten)
            };
            let cell_y = y * chh;
            // Dim rain falling through the figure's negative space, keyed on the
            // cell center so the multiplier is stable regardless of glyph shape.
            if let Some(a) = atten {
                let m = a.mult_at(cell_x + cw / 2, cell_y + chh / 2);
                r *= m;
                gr *= m;
                b *= m;
            }
            blit(buf, width, height, cell_x + g.left, cell_y + g.top, g, r as u32, gr as u32, b as u32);
        }
    }
}

/// Paint the hidden "operator view" image: a faint glyph in every grid cell whose
/// mask luminance is non-trivial, at brightness `weight * intensity` in the trail
/// color. Because `blit` is additive, the bright rain heads that fall across these
/// cells light them up — so the image surfaces in motion, not as a static picture.
#[allow(clippy::too_many_arguments)]
fn paint_ghost(
    buf: &mut [u32],
    width: usize,
    height: usize,
    glyphs: &[GlyphBmp],
    weights: &[f32],
    idx: &[u8],
    cell_w: usize,
    cell_h: usize,
    cols: usize,
    rows: usize,
    trail: (u8, u8, u8),
    intensity: f32,
) {
    let cw = cell_w as i32;
    let chh = cell_h as i32;
    for cy in 0..rows {
        for cx in 0..cols {
            let cell = cy * cols + cx;
            let w = weights[cell];
            if w <= 0.06 {
                continue; // below the noise floor — leave it to the rain
            }
            let inten = w * intensity;
            let (r, g, b) = (
                (trail.0 as f32 * inten) as u32,
                (trail.1 as f32 * inten) as u32,
                (trail.2 as f32 * inten) as u32,
            );
            if r == 0 && g == 0 && b == 0 {
                continue;
            }
            let glyph = &glyphs[idx[cell] as usize];
            if glyph.cov.is_empty() {
                continue;
            }
            blit(buf, width, height, cx as i32 * cw + glyph.left, cy as i32 * chh + glyph.top, glyph, r, g, b);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn blit(buf: &mut [u32], width: usize, height: usize, ox: i32, oy: i32, g: &GlyphBmp, r: u32, gr: u32, b: u32) {
    for gy in 0..g.h as i32 {
        let py = oy + gy;
        if py < 0 || py >= height as i32 {
            continue;
        }
        for gx in 0..g.w as i32 {
            let a = g.cov[(gy as usize) * g.w + gx as usize] as u32;
            if a == 0 {
                continue;
            }
            let px = ox + gx;
            if px < 0 || px >= width as i32 {
                continue;
            }
            let idx = py as usize * width + px as usize;
            let old = buf[idx];
            // additive blend (saturating) so overlapping depth layers brighten
            let rr = (((old >> 16) & 0xFF) + r * a / 255).min(255);
            let gg = (((old >> 8) & 0xFF) + gr * a / 255).min(255);
            let bb = ((old & 0xFF) + b * a / 255).min(255);
            buf[idx] = (rr << 16) | (gg << 8) | bb;
        }
    }
}

/// Additive bloom: blur the bright pixels and add the soft halo back, in place.
/// A cheap CPU approximation done at 1/4 resolution — this is what gives the
/// "neo" look its glow, and it's only possible because we own real pixels.
fn apply_bloom(buf: &mut [u32], w: usize, h: usize) {
    const D: usize = 4; // downsample factor
    const RADIUS: usize = 3; // blur radius in downsampled pixels
    const GAIN: f32 = 0.6; // how strongly the halo adds back
    let (lw, lh) = (w.div_ceil(D), h.div_ceil(D));
    let mut lr = vec![0f32; lw * lh];
    let mut lg = vec![0f32; lw * lh];
    let mut lb = vec![0f32; lw * lh];
    // downsample by max — keeps bright isolated leaders glowing
    for y in 0..h {
        let ly = y / D;
        for x in 0..w {
            let px = buf[y * w + x];
            let li = ly * lw + x / D;
            lr[li] = lr[li].max(((px >> 16) & 0xFF) as f32);
            lg[li] = lg[li].max(((px >> 8) & 0xFF) as f32);
            lb[li] = lb[li].max((px & 0xFF) as f32);
        }
    }
    for chan in [&mut lr, &mut lg, &mut lb] {
        box_blur(chan, lw, lh, RADIUS);
        box_blur(chan, lw, lh, RADIUS); // two passes ≈ gaussian
    }
    for y in 0..h {
        let ly = y / D;
        for x in 0..w {
            let li = ly * lw + x / D;
            let i = y * w + x;
            let px = buf[i];
            let r = (((px >> 16) & 0xFF) as f32 + lr[li] * GAIN).min(255.0) as u32;
            let g = (((px >> 8) & 0xFF) as f32 + lg[li] * GAIN).min(255.0) as u32;
            let b = ((px & 0xFF) as f32 + lb[li] * GAIN).min(255.0) as u32;
            buf[i] = (r << 16) | (g << 8) | b;
        }
    }
}

/// Separable normalized box blur (edges clamped).
fn box_blur(data: &mut [f32], w: usize, h: usize, r: usize) {
    if r == 0 || w == 0 || h == 0 {
        return;
    }
    let win = (2 * r + 1) as f32;
    let mut tmp = vec![0f32; w * h];
    for y in 0..h {
        let row = y * w;
        for x in 0..w {
            let mut sum = 0.0;
            for k in 0..=2 * r {
                let xx = (x + k).saturating_sub(r).min(w - 1);
                sum += data[row + xx];
            }
            tmp[row + x] = sum / win;
        }
    }
    for x in 0..w {
        for y in 0..h {
            let mut sum = 0.0;
            for k in 0..=2 * r {
                let yy = (y + k).saturating_sub(r).min(h - 1);
                sum += tmp[yy * w + x];
            }
            data[y * w + x] = sum / win;
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("wlmatrix")
            .with_fullscreen(Some(Fullscreen::Borderless(None)));
        let window = Rc::new(event_loop.create_window(attrs).expect("create_window"));
        let context = softbuffer::Context::new(window.clone()).expect("sb context");
        let surface = softbuffer::Surface::new(&context, window.clone()).expect("sb surface");
        self.window = Some(window.clone());
        self.surface = Some(surface);
        let size = window.inner_size();
        self.rebuild_grid(size.width.max(1) as usize, size.height.max(1) as usize);
        self.last = Instant::now();
        self.next_frame = Instant::now();
        self.start = Instant::now();
        window.request_redraw();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        // Ignore input for the first moments so a spurious startup CursorMoved
        // (emitted when the fullscreen surface maps) doesn't exit immediately.
        let armed = self.start.elapsed() > Duration::from_millis(700);
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::CursorMoved { .. }
            | WindowEvent::MouseInput { .. }
            | WindowEvent::MouseWheel { .. } => {
                if armed {
                    event_loop.exit();
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if armed && event.state == ElementState::Pressed {
                    event_loop.exit();
                }
            }
            WindowEvent::Resized(size) => {
                if size.width > 0 && size.height > 0 {
                    self.rebuild_grid(size.width as usize, size.height as usize);
                }
            }
            WindowEvent::RedrawRequested => {
                let now = Instant::now();
                let dt = (now - self.last).as_secs_f32().min(0.1);
                self.last = now;
                if self.inited_grid {
                    self.step(dt);
                    self.render();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        if now >= self.next_frame {
            self.next_frame = now + Duration::from_millis(1000 / self.fps.max(1));
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_frame));
    }
}

// ============================== CLI / entry ==============================

const HELP: &str = "\
wlmatrix — native Wayland Matrix-rain screensaver (CPU-rendered, no GPU)

USAGE:
    wlmatrix [OPTIONS]

OPTIONS:
    -h, --help              Show this help and exit
    -V, --version           Show version and exit
        --config <FILE>     Use a specific config file
        --fps <N>           Frames per second
        --font-size <PX>    Glyph size (bigger = sparser rain)
        --speed <MIN-MAX>   Fall speed range in rows/sec (e.g. 4-20)
        --color <C>         green|amber|cyan|red|purple|white|#RRGGBB
        --charset <C>       ascii|alnum|binary|digits|katakana|<literal chars>
        --style <S>         classic | neo  (neo = bloom + katakana)
        --glow / --no-glow  Toggle additive bloom
        --depth <1-4>       Parallax rain layers (1 = flat, 3 = deep)
        --mask <FILE.png>   Hide an image in the rain (any PNG; luminance = brightness)
        --mask-text <TEXT>  Hide a line of text in the rain
        --mask-intensity <0..1>  How strongly the hidden image glows (default 0.85)
        --mask-contrast <0..1>   Dim the rain OUTSIDE the figure so it pops (default 0)
        --shot <FILE>       Render one frame to a PNG and exit
        --gif  <FILE>       Render an animated looping GIF and exit

Settings load from wlmatrix/config.toml in your OS config dir (~/.config on
Linux, ~/Library/Application Support on macOS, %APPDATA% on Windows); CLI flags
override the file. With no options it runs fullscreen and exits on any input
(on Linux, normally launched on idle by the wl-screensaver daemon).
";

fn die(msg: &str) -> ! {
    eprintln!("wlmatrix: {msg}");
    std::process::exit(2);
}

/// Consume the next CLI argument (the value for a flag), or exit with `label`.
fn next_val(args: &[String], i: &mut usize, label: &str) -> String {
    *i += 1;
    args.get(*i).cloned().unwrap_or_else(|| die(label))
}

/// Render one steady-state frame (16:9) straight to a PNG — no window needed.
fn render_shot(cfg: &Config, path: &str) {
    let (w, h) = (1600usize, 900usize);
    let mut app = App::new(cfg);
    app.rebuild_grid(w, h);
    for _ in 0..220 {
        app.step(0.05);
    }
    let mut buf = vec![0u32; w * h];
    render_layers(
        &mut buf, w, h, &app.layers, app.head, app.trail, cfg.glow,
        &app.ghost_weights, &app.ghost_idx, app.mask_intensity, app.mask_contrast,
    );

    let mut rgb = Vec::with_capacity(w * h * 3);
    for px in &buf {
        rgb.push((px >> 16) as u8);
        rgb.push((px >> 8) as u8);
        rgb.push(*px as u8);
    }
    let file = std::fs::File::create(path).expect("create png file");
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().expect("png header").write_image_data(&rgb).expect("png data");
    println!("wrote {path} ({w}x{h})");
}

/// Render an animated, looping GIF of the rain — no window/screen-capture needed.
/// Uses a denser (smaller) font so the rain stays dense at GIF resolution, and
/// respects the configured colors.
fn render_gif(cfg: &Config, path: &str) {
    use std::borrow::Cow;
    let (w, h) = (720usize, 405usize);
    let (frames, delay, levels) = (48, 5u16, 32usize);

    let mut gcfg = cfg.clone();
    gcfg.font_px = gcfg.font_px.min(15.0);
    let mut app = App::new(&gcfg);
    app.rebuild_grid(w, h);
    for _ in 0..120 {
        app.step(0.05);
    }

    // 64-color global palette: 0=black, 1=head, 2..=(1+levels) trail ramp.
    let mut palette = vec![0u8; 64 * 3];
    palette[3..6].copy_from_slice(&[cfg.head.0, cfg.head.1, cfg.head.2]);
    for i in 0..levels {
        let f = (i + 1) as f32 / levels as f32;
        let p = (2 + i) * 3;
        palette[p] = (cfg.trail.0 as f32 * f) as u8;
        palette[p + 1] = (cfg.trail.1 as f32 * f) as u8;
        palette[p + 2] = (cfg.trail.2 as f32 * f) as u8;
    }

    let file = std::fs::File::create(path).expect("create gif file");
    let mut encoder =
        gif::Encoder::new(std::io::BufWriter::new(file), w as u16, h as u16, &palette).expect("gif encoder");
    encoder.set_repeat(gif::Repeat::Infinite).expect("gif repeat");

    let mut buf = vec![0u32; w * h];
    for _ in 0..frames {
        app.step(0.05);
        render_layers(
            &mut buf, w, h, &app.layers, app.head, app.trail, cfg.glow,
            &app.ghost_weights, &app.ghost_idx, app.mask_intensity, app.mask_contrast,
        );
        let mut indexed = vec![0u8; w * h];
        for (i, px) in buf.iter().enumerate() {
            let r = (px >> 16) & 0xFF;
            let g = (px >> 8) & 0xFF;
            let b = px & 0xFF;
            indexed[i] = if r == 0 && g == 0 && b == 0 {
                0
            } else if r > 120 && b > 120 {
                1 // light leader
            } else {
                let inten = r.max(g).max(b) as usize;
                2 + (inten * levels / 256).min(levels - 1) as u8
            };
        }
        let mut frame = gif::Frame::default();
        frame.width = w as u16;
        frame.height = h as u16;
        frame.buffer = Cow::Owned(indexed);
        frame.delay = delay;
        encoder.write_frame(&frame).expect("gif frame");
    }
    println!("wrote {path} ({w}x{h}, {frames} frames)");
}

enum Action {
    Run,
    Shot(String),
    Gif(String),
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // Load the config file first (CLI --config overrides the default path).
    let cfg_path = args
        .iter()
        .position(|a| a == "--config")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .or_else(default_config_path);
    let mut cfg = Config::default();
    if let Some(p) = &cfg_path {
        if let Ok(text) = std::fs::read_to_string(p) {
            parse_config_text(&text, &mut cfg);
        }
    }

    // Then apply CLI flags on top (they win over the file).
    let mut action = Action::Run;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                return;
            }
            "-V" | "--version" => {
                println!("wlmatrix {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            "--config" => i += 1, // value already consumed when locating the file
            "--fps" => {
                let v = next_val(&args, &mut i, "--fps requires a number");
                apply_key(&mut cfg, "fps", &v);
            }
            "--font-size" => {
                let v = next_val(&args, &mut i, "--font-size requires a number");
                apply_key(&mut cfg, "font_size", &v);
            }
            "--speed" => {
                let v = next_val(&args, &mut i, "--speed requires MIN-MAX");
                apply_key(&mut cfg, "speed", &v);
            }
            "--color" => {
                let v = next_val(&args, &mut i, "--color requires a value");
                apply_key(&mut cfg, "color", &v);
            }
            "--charset" => {
                let v = next_val(&args, &mut i, "--charset requires a value");
                apply_key(&mut cfg, "charset", &v);
            }
            "--style" => {
                let v = next_val(&args, &mut i, "--style requires classic|neo");
                apply_key(&mut cfg, "style", &v);
            }
            "--glow" => apply_key(&mut cfg, "glow", "true"),
            "--no-glow" => apply_key(&mut cfg, "glow", "false"),
            "--depth" => {
                let v = next_val(&args, &mut i, "--depth requires 1-4");
                apply_key(&mut cfg, "depth", &v);
            }
            "--mask" => {
                let v = next_val(&args, &mut i, "--mask requires a PNG path");
                apply_key(&mut cfg, "mask", &v);
            }
            "--mask-text" => {
                let v = next_val(&args, &mut i, "--mask-text requires text");
                apply_key(&mut cfg, "mask_text", &v);
            }
            "--mask-intensity" => {
                let v = next_val(&args, &mut i, "--mask-intensity requires 0..1");
                apply_key(&mut cfg, "mask_intensity", &v);
            }
            "--mask-contrast" => {
                let v = next_val(&args, &mut i, "--mask-contrast requires 0..1");
                apply_key(&mut cfg, "mask_contrast", &v);
            }
            "--shot" => action = Action::Shot(next_val(&args, &mut i, "--shot requires a file path")),
            "--gif" => action = Action::Gif(next_val(&args, &mut i, "--gif requires a file path")),
            other => die(&format!("unknown argument '{other}'. Try --help.")),
        }
        i += 1;
    }

    match action {
        Action::Shot(p) => render_shot(&cfg, &p),
        Action::Gif(p) => render_gif(&cfg, &p),
        Action::Run => {
            let event_loop = EventLoop::new().expect("event loop");
            event_loop.set_control_flow(ControlFlow::Poll);
            let mut app = App::new(&cfg);
            let _ = event_loop.run_app(&mut app);
        }
    }
}
