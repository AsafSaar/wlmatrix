// wlmatrix — native Wayland Matrix digital-rain screensaver.
//
// Renders pixels on the CPU into a wl_shm buffer (softbuffer) — deliberately NO
// OpenGL/GPU, because the GL/GLX path is broken on this box (every xscreensaver
// hack and GL window failed to fullscreen here). winit gives us a real Wayland
// fullscreen surface. Any keypress / mouse movement / click exits, so it
// dismisses instantly. The wl-screensaver daemon launches it on idle.
//
// Configuration: ~/.config/wlmatrix/config.toml (flat key = value, TOML-compatible),
// overridable by CLI flags. See HELP and load_config().

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use ab_glyph::{Font, FontVec, Glyph, Point, ScaleFont};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Fullscreen, Window, WindowId};

const FONT_CANDIDATES: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansMono-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoMono-Regular.ttf",
];
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
        }
    }
}

/// `~/.config/wlmatrix/config.toml` (respects $XDG_CONFIG_HOME).
fn default_config_path() -> Option<PathBuf> {
    if let Ok(x) = std::env::var("XDG_CONFIG_HOME") {
        if !x.is_empty() {
            return Some(Path::new(&x).join("wlmatrix/config.toml"));
        }
    }
    std::env::var("HOME")
        .ok()
        .map(|h| Path::new(&h).join(".config/wlmatrix/config.toml"))
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
        // half-width katakana — needs a CJK-capable font (set `font` in config)
        "katakana" => ('\u{FF66}'..='\u{FF9D}').collect(),
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

struct Column {
    head: f32,
    speed: f32, // rows per second
    len: i32,
    chars: Vec<u8>, // index into the glyph set for each row
}

type Surf = softbuffer::Surface<Rc<Window>, Rc<Window>>;

struct App {
    window: Option<Rc<Window>>,
    surface: Option<Surf>,
    glyphs: Vec<GlyphBmp>, // one per charset char
    cell_w: usize,
    cell_h: usize,
    cols: usize,
    rows: usize,
    columns: Vec<Column>,
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
}

impl App {
    fn new(cfg: &Config) -> App {
        let path = cfg
            .font_path
            .clone()
            .filter(|p| Path::new(p).exists())
            .or_else(|| {
                FONT_CANDIDATES
                    .iter()
                    .find(|p| Path::new(p).exists())
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(|| FONT_CANDIDATES[0].to_string());
        let data = std::fs::read(&path).expect("cannot read font file");
        let font = FontVec::try_from_vec(data).expect("invalid font");
        let scaled = font.as_scaled(cfg.font_px);
        let ascent = scaled.ascent();
        let advance = scaled.h_advance(font.glyph_id('M'));
        let cell_w = advance.ceil().max(1.0) as usize;
        let cell_h = (scaled.ascent() - scaled.descent()).ceil().max(1.0) as usize;

        let mut glyphs = Vec::new();
        for ch in cfg.charset.chars() {
            let glyph: Glyph = font
                .glyph_id(ch)
                .with_scale_and_position(cfg.font_px, Point { x: 0.0, y: 0.0 });
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

        App {
            window: None,
            surface: None,
            glyphs,
            cell_w,
            cell_h,
            cols: 0,
            rows: 0,
            columns: Vec::new(),
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
        }
    }

    fn rand_speed(&mut self) -> f32 {
        self.speed_min + self.rng.frac() * (self.speed_max - self.speed_min)
    }

    fn rebuild_grid(&mut self, width: usize, height: usize) {
        self.cols = (width / self.cell_w).max(1);
        self.rows = (height / self.cell_h).max(1);
        let rows = self.rows;
        let nglyphs = self.glyphs.len() as u32;
        let mut columns = Vec::with_capacity(self.cols);
        for _ in 0..self.cols {
            let mut chars = vec![0u8; rows];
            for c in chars.iter_mut() {
                *c = self.rng.below(nglyphs) as u8;
            }
            let speed = self.rand_speed();
            columns.push(Column {
                head: -(self.rng.below(rows as u32) as f32),
                speed,
                len: 6 + self.rng.below((rows as u32 / 2).max(7)) as i32,
                chars,
            });
        }
        self.columns = columns;
        self.inited_grid = true;
    }

    fn step(&mut self, dt: f32) {
        let rows = self.rows as i32;
        let nglyphs = self.glyphs.len() as u32;
        for idx in 0..self.columns.len() {
            let (old_i, new_i) = {
                let col = &mut self.columns[idx];
                let old_i = col.head.floor() as i32;
                col.head += col.speed * dt;
                (old_i, col.head.floor() as i32)
            };
            for y in (old_i + 1)..=new_i {
                if y >= 0 && y < rows {
                    let c = (self.rng.next_u64() % nglyphs as u64) as u8;
                    self.columns[idx].chars[y as usize] = c;
                }
            }
            if new_i - self.columns[idx].len > rows {
                let speed = self.rand_speed();
                let len = 6 + self.rng.below((rows as u32 / 2).max(7)) as i32;
                let head = -(self.rng.below(rows as u32 + 1) as f32) - len as f32;
                let col = &mut self.columns[idx];
                col.head = head;
                col.speed = speed;
                col.len = len;
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
        paint_frame(
            &mut buf, width, height, &self.glyphs, &self.columns, self.cell_w, self.cell_h,
            self.rows, self.head, self.trail,
        );
        let _ = buf.present();
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
) {
    for px in buf.iter_mut() {
        *px = 0; // clear to black
    }
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
            let (r, gr, b) = if k == 0 {
                (head.0 as u32, head.1 as u32, head.2 as u32)
            } else {
                let t = 1.0 - (k as f32 / col.len as f32);
                let inten = (0.25 + 0.75 * t).min(1.0);
                (
                    (trail.0 as f32 * inten) as u32,
                    (trail.1 as f32 * inten) as u32,
                    (trail.2 as f32 * inten) as u32,
                )
            };
            let cell_y = y * chh;
            blit(buf, width, height, cell_x + g.left, cell_y + g.top, g, r, gr, b);
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
            let rr = (r * a / 255) & 0xFF;
            let gg = (gr * a / 255) & 0xFF;
            let bb = (b * a / 255) & 0xFF;
            buf[py as usize * width + px as usize] = (rr << 16) | (gg << 8) | bb;
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
        --shot <FILE>       Render one frame to a PNG and exit
        --gif  <FILE>       Render an animated looping GIF and exit

Settings load from ~/.config/wlmatrix/config.toml (or $XDG_CONFIG_HOME);
CLI flags override the file. With no options it runs fullscreen and exits on
any input, normally launched on idle by the wl-screensaver daemon.
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
    paint_frame(&mut buf, w, h, &app.glyphs, &app.columns, app.cell_w, app.cell_h, app.rows, app.head, app.trail);

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
    let (w, h) = (800usize, 450usize);
    let (frames, delay, levels) = (60, 5u16, 32usize);

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
        paint_frame(&mut buf, w, h, &app.glyphs, &app.columns, app.cell_w, app.cell_h, app.rows, app.head, app.trail);
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
