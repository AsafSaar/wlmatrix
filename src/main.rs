// wlmatrix — native Wayland Matrix digital-rain screensaver.
//
// Renders pixels on the CPU into a wl_shm buffer (softbuffer) — deliberately NO
// OpenGL/GPU, because the GL/GLX path is broken on this box (every xscreensaver
// hack and GL window failed to fullscreen here). winit gives us a real Wayland
// fullscreen surface. Any keypress / mouse movement / click exits, so it
// dismisses instantly. The wl-screensaver daemon launches it on idle.

use std::num::NonZeroU32;
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
const CHARSET: &str =
    "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789@#$%&*+-/<>=?!:";
const FPS: u64 = 30;
const FONT_PX: f32 = 26.0; // glyph pixel height

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
        (self.next_u64() % n as u64) as u32
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
    chars: Vec<u8>, // index into CHARSET for each row
}

type Surf = softbuffer::Surface<Rc<Window>, Rc<Window>>;

struct App {
    window: Option<Rc<Window>>,
    surface: Option<Surf>,
    glyphs: Vec<GlyphBmp>, // one per CHARSET char
    cell_w: usize,
    cell_h: usize,
    cols: usize,
    rows: usize,
    columns: Vec<Column>,
    rng: Rng,
    last: Instant,
    next_frame: Instant,
    inited_grid: bool,
    start: Instant, // input is ignored briefly after launch (avoids spurious startup-event exit)
}

impl App {
    fn new() -> App {
        // load font
        let path = FONT_CANDIDATES
            .iter()
            .find(|p| std::path::Path::new(p).exists())
            .copied()
            .unwrap_or(FONT_CANDIDATES[0]);
        let data = std::fs::read(path).expect("cannot read font file");
        let font = FontVec::try_from_vec(data).expect("invalid font");
        let scaled = font.as_scaled(FONT_PX);
        let ascent = scaled.ascent();
        let advance = scaled.h_advance(font.glyph_id('M'));
        let cell_w = advance.ceil().max(1.0) as usize;
        let cell_h = (scaled.ascent() - scaled.descent()).ceil().max(1.0) as usize;

        let mut glyphs = Vec::new();
        for ch in CHARSET.chars() {
            let glyph: Glyph = font.glyph_id(ch).with_scale_and_position(FONT_PX, Point { x: 0.0, y: 0.0 });
            if let Some(outline) = font.outline_glyph(glyph) {
                let b = outline.px_bounds();
                let gw = b.width().ceil() as usize;
                let gh = b.height().ceil() as usize;
                let mut cov = vec![0u8; gw * gh];
                outline.draw(|x, y, c| {
                    let xi = x as usize;
                    let yi = y as usize;
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
        }
    }

    fn rebuild_grid(&mut self, width: usize, height: usize) {
        self.cols = (width / self.cell_w).max(1);
        self.rows = (height / self.cell_h).max(1);
        let rows = self.rows;
        self.columns = (0..self.cols)
            .map(|_| {
                let mut chars = vec![0u8; rows];
                for c in chars.iter_mut() {
                    *c = self.rng.below(CHARSET.chars().count() as u32) as u8;
                }
                Column {
                    head: -(self.rng.below(rows as u32) as f32),
                    speed: 6.0 + self.rng.frac() * 18.0,
                    len: 6 + self.rng.below((rows as u32 / 2).max(7)) as i32,
                    chars,
                }
            })
            .collect();
        self.inited_grid = true;
    }

    fn step(&mut self, dt: f32) {
        let rows = self.rows as i32;
        let nchars = CHARSET.chars().count() as u32;
        for col in self.columns.iter_mut() {
            let old_i = col.head.floor() as i32;
            col.head += col.speed * dt;
            let new_i = col.head.floor() as i32;
            for y in (old_i + 1)..=new_i {
                if y >= 0 && y < rows {
                    col.chars[y as usize] = (self.rng.next_u64() % nchars as u64) as u8;
                }
            }
            if new_i - col.len > rows {
                col.head = -(self.rng.below(rows as u32 + 1) as f32) - col.len as f32;
                col.speed = 6.0 + self.rng.frac() * 18.0;
                col.len = 6 + self.rng.below((rows as u32 / 2).max(7)) as i32;
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
        let Ok(()) = surface
            .resize(
                NonZeroU32::new(size.width).unwrap(),
                NonZeroU32::new(size.height).unwrap(),
            )
            .map_err(|_| ())
        else {
            return;
        };
        let mut buf = match surface.buffer_mut() {
            Ok(b) => b,
            Err(_) => return,
        };
        paint_frame(
            &mut buf,
            width,
            height,
            &self.glyphs,
            &self.columns,
            self.cell_w,
            self.cell_h,
            self.rows,
        );
        let _ = buf.present();
    }
}

/// Draw one frame of rain into `buf` (pixels are 0x00RRGGBB). Shared by the live
/// renderer and by `--shot` (which renders into a plain Vec for PNG export).
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
            // head is bright white; the trail fades from bright to dark green
            let (r, gr, b) = if k == 0 {
                (220u32, 255, 220)
            } else {
                let t = 1.0 - (k as f32 / col.len as f32);
                let inten = (0.25 + 0.75 * t).min(1.0);
                (0, (255.0 * inten) as u32, (40.0 * inten) as u32)
            };
            let cell_y = y * chh;
            blit(buf, width, height, cell_x + g.left, cell_y + g.top, g, r, gr, b);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn blit(
    buf: &mut [u32],
    width: usize,
    height: usize,
    ox: i32,
    oy: i32,
    g: &GlyphBmp,
    r: u32,
    gr: u32,
    b: u32,
) {
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
            // any real input dismisses the screensaver
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
            self.next_frame = now + Duration::from_millis(1000 / FPS);
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_frame));
    }
}

const HELP: &str = "\
wlmatrix — native Wayland Matrix-rain screensaver (CPU-rendered, no GPU)

USAGE:
    wlmatrix [OPTIONS]

OPTIONS:
    -h, --help           Show this help and exit
    -V, --version        Show version and exit
        --shot <FILE>    Render one frame to a PNG and exit (for previews/README)

With no options it runs fullscreen and exits on any key or mouse input. It is
normally launched on idle by the companion wl-screensaver daemon.
";

/// Render one steady-state frame (16:9) straight to a PNG — no window needed.
fn render_shot(path: &str) {
    let (w, h) = (1600usize, 900usize);
    let mut app = App::new();
    app.rebuild_grid(w, h);
    for _ in 0..220 {
        app.step(0.05); // let the rain reach a full-screen steady state
    }
    let mut buf = vec![0u32; w * h];
    paint_frame(&mut buf, w, h, &app.glyphs, &app.columns, app.cell_w, app.cell_h, app.rows);

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
    enc.write_header()
        .expect("png header")
        .write_image_data(&rgb)
        .expect("png data");
    println!("wrote {path} ({w}x{h})");
}

fn main() {
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                return;
            }
            "-V" | "--version" => {
                println!("wlmatrix {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            "--shot" => {
                let path = args.next().unwrap_or_else(|| {
                    eprintln!("--shot requires a file path");
                    std::process::exit(2);
                });
                render_shot(&path);
                return;
            }
            other => {
                eprintln!("wlmatrix: unknown argument '{other}'. Try --help.");
                std::process::exit(2);
            }
        }
    }

    let event_loop = EventLoop::new().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App::new();
    let _ = event_loop.run_app(&mut app);
}
