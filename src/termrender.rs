
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use alacritty_terminal::{
    event::{Event, EventListener, VoidListener, WindowSize},
    grid::Dimensions,
    index::{Column, Line},
    term::{cell::Flags, Config, Term},
    vte::{
        self,
        ansi::{Color, NamedColor, Rgb},
    },
};

pub(crate) const TERM_COLS: usize = 120;
pub(crate) const TERM_ROWS: usize = 40;
const FONT_PX: f32 = 16.0;
const PAD: u32 = 6;
const COL_STEP: usize = 20;
const ROW_STEP: usize = 8;
const MIN_COLS: u32 = 60;
const MIN_ROWS: u32 = 12;

const BG: [u8; 3] = [0x0b, 0x0e, 0x14];
const FG: [u8; 3] = [0xe6, 0xe6, 0xe6];

#[derive(Debug, Clone, Copy)]
struct TermDims;

impl Dimensions for TermDims {
    fn total_lines(&self) -> usize {
        TERM_ROWS
    }

    fn screen_lines(&self) -> usize {
        TERM_ROWS
    }

    fn columns(&self) -> usize {
        TERM_COLS
    }
}

#[derive(Clone, Default)]
pub(crate) struct CollectingListener {
    pub writes: Arc<Mutex<Vec<String>>>,
}

impl EventListener for CollectingListener {
    fn send_event(&self, event: Event) {
        match event {
            Event::PtyWrite(s) => self.writes.lock().unwrap().push(s),
            Event::TextAreaSizeRequest(f) => {
                let size = WindowSize {
                    num_cols: TERM_COLS as u16,
                    num_lines: TERM_ROWS as u16,
                    cell_width: 10,
                    cell_height: 20,
                };
                self.writes.lock().unwrap().push(f(size));
            },
            Event::ColorRequest(idx, f) => {
                let rgb = Rgb { r: 0, g: 0, b: 0 };
                let _ = idx;
                self.writes.lock().unwrap().push(f(rgb));
            },
            _ => {},
        }
    }
}

pub(crate) fn new_collecting_term() -> (Term<CollectingListener>, vte::ansi::Processor, Arc<Mutex<Vec<String>>>) {
    let writes = Arc::new(Mutex::new(Vec::new()));
    let listener = CollectingListener { writes: writes.clone() };
    let config = Config {
        scrolling_history: TERM_ROWS,
        kitty_keyboard: true,
        ..Default::default()
    };
    let term = Term::new(config, &TermDims, listener);
    let processor = vte::ansi::Processor::new();
    (term, processor, writes)
}

pub(crate) fn new_bytes_since(prev: &str, cur: &str) -> String {
    if cur.starts_with(prev) {
        return cur[prev.len()..].to_string();
    }
    let max = prev.len().min(cur.len());
    for k in (0..=max).rev() {
        if k == 0 {
            return cur.to_string();
        }
        if prev[prev.len() - k..] == cur[..k] {
            return cur[k..].to_string();
        }
    }
    cur.to_string()
}

pub(crate) struct TermFonts {
    regular: fontdue::Font,
    bold: fontdue::Font,
    cell_w: u32,
    cell_h: u32,
    ascent: i32,
}

pub(crate) fn system_font_bytes(spec: &str) -> Option<Vec<u8>> {
    let fc = crate::util::tool_path("fc-match")?;
    let out = std::process::Command::new(&fc)
        .args([spec, "--format=%{file}"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if p.is_empty() {
        return None;
    }
    std::fs::read(&p).ok().filter(|b| !b.is_empty())
}

impl TermFonts {
    pub fn load(regular_bytes: &[u8], bold_bytes: &[u8]) -> Option<Self> {
        let regular =
            fontdue::Font::from_bytes(regular_bytes, fontdue::FontSettings::default()).ok()?;
        let bold = fontdue::Font::from_bytes(bold_bytes, fontdue::FontSettings::default()).ok()?;
        let cell_w = regular.metrics('M', FONT_PX).advance_width.ceil().max(1.0) as u32;
        let (ascent, descent) = match regular.horizontal_line_metrics(FONT_PX) {
            Some(lm) => (lm.ascent.ceil() as i32, (-lm.descent).ceil() as u32),
            None => ((FONT_PX * 0.8).ceil() as i32, (FONT_PX * 0.25).ceil() as u32),
        };
        let cell_h = (ascent as u32 + descent + 4).max(1);
        Some(Self { regular, bold, cell_w, cell_h, ascent })
    }
}

fn named_color(n: NamedColor) -> [u8; 3] {
    match n {
        NamedColor::Black => [0x00, 0x00, 0x00],
        NamedColor::Red => [0xcd, 0x00, 0x00],
        NamedColor::Green => [0x00, 0xcd, 0x00],
        NamedColor::Yellow => [0xcd, 0xcd, 0x00],
        NamedColor::Blue => [0x00, 0x00, 0xee],
        NamedColor::Magenta => [0xcd, 0x00, 0xcd],
        NamedColor::Cyan => [0x00, 0xcd, 0xcd],
        NamedColor::White => [0xe5, 0xe5, 0xe5],
        NamedColor::BrightBlack => [0x7f, 0x7f, 0x7f],
        NamedColor::BrightRed => [0xff, 0x00, 0x00],
        NamedColor::BrightGreen => [0x00, 0xff, 0x00],
        NamedColor::BrightYellow => [0xff, 0xff, 0x00],
        NamedColor::BrightBlue => [0x5c, 0x5c, 0xff],
        NamedColor::BrightMagenta => [0xff, 0x00, 0xff],
        NamedColor::BrightCyan => [0x00, 0xff, 0xff],
        NamedColor::BrightWhite => [0xff, 0xff, 0xff],
        NamedColor::Foreground => FG,
        NamedColor::Background => BG,
        NamedColor::Cursor => FG,
        NamedColor::DimBlack => dim([0x00, 0x00, 0x00]),
        NamedColor::DimRed => dim([0xcd, 0x00, 0x00]),
        NamedColor::DimGreen => dim([0x00, 0xcd, 0x00]),
        NamedColor::DimYellow => dim([0xcd, 0xcd, 0x00]),
        NamedColor::DimBlue => dim([0x00, 0x00, 0xee]),
        NamedColor::DimMagenta => dim([0xcd, 0x00, 0xcd]),
        NamedColor::DimCyan => dim([0x00, 0xcd, 0xcd]),
        NamedColor::DimWhite => dim([0xe5, 0xe5, 0xe5]),
        NamedColor::DimForeground => dim(FG),
        _ => FG,
    }
}

/// Real terminals draw bold text in the bright color variants (only the 8
/// normal colors change; bright/indexed/rgb/fg/bg stay as they are).
pub(crate) fn brighten(color: Color) -> Color {
    match color {
        Color::Named(n) => Color::Named(match n {
            NamedColor::Black => NamedColor::BrightBlack,
            NamedColor::Red => NamedColor::BrightRed,
            NamedColor::Green => NamedColor::BrightGreen,
            NamedColor::Yellow => NamedColor::BrightYellow,
            NamedColor::Blue => NamedColor::BrightBlue,
            NamedColor::Magenta => NamedColor::BrightMagenta,
            NamedColor::Cyan => NamedColor::BrightCyan,
            NamedColor::White => NamedColor::BrightWhite,
            other => other,
        }),
        other => other,
    }
}

pub(crate) fn dim(c: [u8; 3]) -> [u8; 3] {
    [(c[0] as u16 * 2 / 3) as u8, (c[1] as u16 * 2 / 3) as u8, (c[2] as u16 * 2 / 3) as u8]
}

pub(crate) fn resolve_color(c: Color) -> [u8; 3] {
    match c {
        Color::Named(n) => named_color(n),
        Color::Indexed(i) => indexed_color(i),
        Color::Spec(rgb) => [rgb.r, rgb.g, rgb.b],
    }
}

pub(crate) fn indexed_color(i: u8) -> [u8; 3] {
    match i {
        0..=15 => {
            const TABLE: [[u8; 3]; 16] = [
                [0x00, 0x00, 0x00],
                [0xcd, 0x00, 0x00],
                [0x00, 0xcd, 0x00],
                [0xcd, 0xcd, 0x00],
                [0x00, 0x00, 0xee],
                [0xcd, 0x00, 0xcd],
                [0x00, 0xcd, 0xcd],
                [0xe5, 0xe5, 0xe5],
                [0x7f, 0x7f, 0x7f],
                [0xff, 0x00, 0x00],
                [0x00, 0xff, 0x00],
                [0xff, 0xff, 0x00],
                [0x5c, 0x5c, 0xff],
                [0xff, 0x00, 0xff],
                [0x00, 0xff, 0xff],
                [0xff, 0xff, 0xff],
            ];
            TABLE[i as usize]
        }
        16..=231 => {
            let i = i - 16;
            let levels = [0x00, 0x5f, 0x87, 0xaf, 0xd7, 0xff];
            [levels[(i / 36) as usize], levels[((i / 6) % 6) as usize], levels[(i % 6) as usize]]
        }
        _ => {
            let v = 8 + (i - 232) * 10;
            [v, v, v]
        }
    }
}

fn blit(
    img: &mut [u8],
    w: u32,
    h: u32,
    x0: i32,
    y0: i32,
    bw: i32,
    bh: i32,
    bmp: &[u8],
    fg: [u8; 3],
) {
    if bw <= 0 || bh <= 0 || bmp.is_empty() {
        return;
    }
    for (i, &a) in bmp.iter().enumerate() {
        if a == 0 {
            continue;
        }
        let bx = x0 + (i as i32 % bw);
        let by = y0 + (i as i32 / bw);
        if bx < 0 || by < 0 || bx >= w as i32 || by >= h as i32 {
            continue;
        }
        let p = ((by as u32 * w + bx as u32) * 3) as usize;
        let a = a as u32;
        for c in 0..3 {
            img[p + c] = ((fg[c] as u32 * a + img[p + c] as u32 * (255 - a)) / 255) as u8;
        }
    }
}

pub(crate) fn emulate_output(output: &[u8]) -> Term<VoidListener> {
    let mut term: Term<VoidListener> = Term::new(
        Config { scrolling_history: TERM_ROWS, ..Default::default() },
        &TermDims,
        VoidListener,
    );
    let mut processor: vte::ansi::Processor = vte::ansi::Processor::new();
    processor.advance(&mut term, output);
    term
}

pub(crate) fn content_region(term: &Term<VoidListener>) -> (usize, usize, usize) {
    let grid = term.grid();
    let mut first: Option<usize> = None;
    let mut last = 0usize;
    let mut cols = 0usize;
    for row in 0..TERM_ROWS {
        let line = &grid[Line(row as i32)];
        let mut row_cols = 0usize;
        for col in 0..TERM_COLS {
            let cell = &line[Column(col)];
            if cell.c != ' ' || resolve_color(cell.bg) != BG {
                row_cols = col + 1;
            }
        }
        if row_cols > 0 {
            if first.is_none() {
                first = Some(row);
            }
            last = row;
            cols = cols.max(row_cols);
        }
    }
    match first {
        Some(f) => (f, last - f + 1, cols),
        None => (0, 0, 0),
    }
}

pub(crate) fn quantize_region(cols: usize, rows: usize, lock: Option<(u32, u32)>) -> (u32, u32) {
    let bucket_cols = ((cols + COL_STEP - 1) / COL_STEP * COL_STEP) as u32;
    let bucket_rows = ((rows + ROW_STEP - 1) / ROW_STEP * ROW_STEP) as u32;
    let mut w = bucket_cols.clamp(MIN_COLS, TERM_COLS as u32);
    let mut h = bucket_rows.clamp(MIN_ROWS, TERM_ROWS as u32);
    if let Some((lw, lh)) = lock {
        w = w.max(lw);
        h = h.max(lh);
    }
    (w, h)
}

pub(crate) fn render_terminal(
    fonts: &TermFonts,
    output: &[u8],
    region: &mut Option<(u32, u32)>,
) -> Option<Vec<u8>> {
    let term = emulate_output(output);
    let (first, rows, cols) = content_region(&term);
    let (cols_q, rows_q) = quantize_region(cols, rows, *region);
    *region = Some((cols_q, rows_q));
    let w = cols_q * fonts.cell_w + PAD * 2;
    let h = rows_q * fonts.cell_h + PAD * 2;
    let content_h = rows.min(rows_q as usize) as u32 * fonts.cell_h;
    let y0 = PAD as i32 + (h.saturating_sub(content_h + PAD * 2) / 2) as i32;
    let mut img = vec![0u8; (w * h * 3) as usize];
    for px in img.chunks_exact_mut(3) {
        px.copy_from_slice(&BG);
    }
    let mut cache: HashMap<(char, bool), (fontdue::Metrics, Vec<u8>)> = HashMap::new();
    let grid = term.grid();
    let rows_draw = rows.min(rows_q as usize);
    let cols_draw = cols.min(cols_q as usize);
    for r in 0..rows_draw {
        let row = first + r;
        if row >= TERM_ROWS {
            break;
        }
        let line = &grid[Line(row as i32)];
        for col in 0..cols_draw.min(TERM_COLS) {
            let cell = &line[Column(col)];
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER)
                || cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            let bold = cell.flags.contains(Flags::BOLD);
            let mut fg_color = cell.fg;
            if bold {
                fg_color = brighten(fg_color);
            }
            let (mut fg, mut bg) = (resolve_color(fg_color), resolve_color(cell.bg));
            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            if cell.flags.contains(Flags::DIM) {
                fg = dim(fg);
            }
            if cell.flags.contains(Flags::HIDDEN) {
                fg = bg;
            }
            let cx = col as i32 * fonts.cell_w as i32 + PAD as i32;
            let cy = y0 + r as i32 * fonts.cell_h as i32;
            if bg != BG {
                let x1 = (cx + fonts.cell_w as i32).min(w as i32);
                let y1 = (cy + fonts.cell_h as i32).min(h as i32);
                for y in cy.max(0)..y1.max(0) {
                    for x in cx.max(0)..x1.max(0) {
                        let p = ((y as u32 * w + x as u32) * 3) as usize;
                        img[p..p + 3].copy_from_slice(&bg);
                    }
                }
            }
            let ch = cell.c;
            if ch == ' ' || ch.is_control() {
                continue;
            }
            let font = if bold { &fonts.bold } else { &fonts.regular };
            let (m, bmp) = cache
                .entry((ch, bold))
                .or_insert_with(|| font.rasterize(ch, FONT_PX));
            let gx = cx + m.xmin;
            let baseline = cy + fonts.ascent;
            let gy = baseline - (m.ymin + m.height as i32);
            blit(&mut img, w, h, gx, gy, m.width as i32, m.height as i32, bmp, fg);
        }
    }
    crate::pngencode::encode_rgb(w, h, &img)
}
