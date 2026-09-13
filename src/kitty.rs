use std::collections::HashMap;

use alacritty_terminal::{
    event::VoidListener,
    term::Term,
    vte::ansi::Processor,
};

pub(crate) const KITTY_MAX_PX: u32 = 4096;
const MAX_B64: usize = 24_000_000;
const MAX_IMAGES: usize = 24;

pub(crate) struct KittyImage {
    pub rgba: Vec<u8>,
    pub pw: u32,
    pub ph: u32,
    pub line: i32,
    pub col: usize,
    pub cols: usize,
    pub rows: usize,
    pub cell_dx: i64,
    pub cell_dy: i64,
    pub px_dx: i64,
    pub px_dy: i64,
    pub key: u64,
    pub pid: Option<i64>,
    pub z: i32,
}

struct Stored {
    rgba: Vec<u8>,
    pw: u32,
    ph: u32,
    cols: Option<usize>,
    rows: Option<usize>,
}

fn num(ctrl: &HashMap<u8, String>, k: u8) -> Option<i64> {
    ctrl.get(&k)?.trim().parse::<i64>().ok()
}

fn img_key(ctrl: &HashMap<u8, String>) -> u64 {
    let i = num(ctrl, b'i').unwrap_or(0).max(0) as u64;
    let big = num(ctrl, b'I').unwrap_or(0).max(0) as u64;
    (i << 32) | (big & 0xffff_ffff)
}

fn parse_ctrl(raw: &[u8]) -> HashMap<u8, String> {
    let mut m = HashMap::new();
    for part in raw.split(|&b| b == b',') {
        if part.len() >= 3 && part[1] == b'=' {
            m.insert(part[0], String::from_utf8_lossy(&part[2..]).into_owned());
        }
    }
    m
}

fn decode_image(
    ctrl: &HashMap<u8, String>,
    data: &[u8],
    files: &HashMap<Vec<u8>, Option<Vec<u8>>>,
) -> Option<(u32, u32, Vec<u8>)> {
    let f = num(ctrl, b'f')?;
    let mut bytes: Vec<u8> = match ctrl.get(&b't').map(|s| s.as_str()).unwrap_or("d") {
        "d" => {
            let clean: Vec<u8> = data
                .iter()
                .copied()
                .filter(|b| {
                    b.is_ascii_alphanumeric() || *b == b'+' || *b == b'/' || *b == b'='
                })
                .collect();
            if clean.len() > MAX_B64 {
                return None;
            }
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD.decode(&clean).ok()?
        }
        "f" | "t" => {
            if data.is_empty() || data.len() > 4096 || data.contains(&0) {
                return None;
            }
            files.get(data).and_then(|o| o.as_ref())?.clone()
        }
        _ => return None,
    };
    match ctrl.get(&b'o').map(|s| s.as_str()).unwrap_or("") {
        "" => {}
        "z" => {
            bytes = miniz_oxide::inflate::decompress_to_vec_zlib(&bytes).ok()?;
        }
        _ => return None,
    }
    match f {
        100 => {
            let (w, h, rgba) = crate::pngencode::decode_png_rgba(&bytes)?;
            Some((w, h, rgba))
        }
        24 | 32 => {
            let s = num(ctrl, b's')?;
            let v = num(ctrl, b'v')?;
            if s <= 0 || v <= 0 || s > KITTY_MAX_PX as i64 || v > KITTY_MAX_PX as i64 {
                return None;
            }
            let ch = if f == 24 { 3u64 } else { 4u64 };
            if bytes.len() as u64 != s as u64 * v as u64 * ch {
                return None;
            }
            if f == 24 {
                let mut o = Vec::with_capacity(bytes.len() / 3 * 4);
                for p in bytes.chunks_exact(3) {
                    o.extend_from_slice(&[p[0], p[1], p[2], 0xff]);
                }
                Some((s as u32, v as u32, o))
            } else {
                Some((s as u32, v as u32, bytes))
            }
        }
        _ => None,
    }
}

fn native_cells(px: u32, cell: u32) -> usize {
    let cell = cell.max(1);
    ((px + cell - 1) / cell).max(1) as usize
}

fn find_esc_us(hay: &[u8]) -> Option<usize> {
    hay.windows(2).position(|w| w[0] == 0x1b && w[1] == b'_')
}

fn find_st(hay: &[u8]) -> Option<(usize, usize)> {
    let mut k = 0usize;
    while k < hay.len() {
        if hay[k] == 0x07 {
            return Some((k, 1));
        }
        if hay[k] == 0x1b && k + 1 < hay.len() && hay[k + 1] == b'\\' {
            return Some((k, 2));
        }
        k += 1;
    }
    None
}

fn display(
    term: &mut Term<VoidListener>,
    proc: &mut Processor,
    ctrl: &HashMap<u8, String>,
    key: u64,
    stored: &HashMap<u64, Stored>,
    placed: &mut Vec<KittyImage>,
    cell_w: u32,
    cell_h: u32,
) {
    let st = match stored.get(&key) {
        Some(s) => s,
        None => {
            if ctrl.contains_key(&b'I') {
                return;
            }
            let id = key >> 32;
            match stored.iter().find(|(k, _)| *k >> 32 == id).map(|(_, v)| v) {
                Some(s) => s,
                None => return,
            }
        }
    };
    let cols = num(ctrl, b'c')
        .filter(|v| *v > 0)
        .map(|v| v as usize)
        .or(st.cols)
        .unwrap_or_else(|| native_cells(st.pw, cell_w));
    let rows = num(ctrl, b'r')
        .filter(|v| *v > 0)
        .map(|v| v as usize)
        .or(st.rows)
        .unwrap_or_else(|| native_cells(st.ph, cell_h));
    if placed.len() >= MAX_IMAGES {
        return;
    }
    let cur = term.grid().cursor.point;
    placed.push(KittyImage {
        rgba: st.rgba.clone(),
        pw: st.pw,
        ph: st.ph,
        line: cur.line.0,
        col: cur.column.0 as usize,
        cols,
        rows,
        cell_dx: num(ctrl, b'x').unwrap_or(0),
        cell_dy: num(ctrl, b'y').unwrap_or(0),
        px_dx: num(ctrl, b'X').unwrap_or(0),
        px_dy: num(ctrl, b'Y').unwrap_or(0),
        key,
        pid: num(ctrl, b'p'),
        z: num(ctrl, b'z').unwrap_or(0).clamp(-1000, 1000) as i32,
    });
    if num(ctrl, b'C').unwrap_or(0) == 1 {
        let nl = b"\r\n".repeat(rows);
        proc.advance(term, &nl);
    }
}

fn handle_kitty(
    term: &mut Term<VoidListener>,
    proc: &mut Processor,
    cmd: &[u8],
    stored: &mut HashMap<u64, Stored>,
    pending: &mut HashMap<u64, (HashMap<u8, String>, Vec<u8>)>,
    placed: &mut Vec<KittyImage>,
    files: &HashMap<Vec<u8>, Option<Vec<u8>>>,
    cell_w: u32,
    cell_h: u32,
) {
    let (raw_ctrl, payload) = match cmd.iter().position(|&b| b == b';') {
        Some(s) => (&cmd[..s], &cmd[s + 1..]),
        None => (cmd, &[][..]),
    };
    let cur = parse_ctrl(raw_ctrl);
    let mut key = img_key(&cur);
    if !cur.contains_key(&b'i') && !cur.contains_key(&b'I') && pending.len() == 1 {
        if let Some(k) = pending.keys().next() {
            key = *k;
        }
    }
    if num(&cur, b'm').unwrap_or(0) == 1 {
        let slot = pending.entry(key).or_insert_with(|| (HashMap::new(), Vec::new()));
        for (k, v) in &cur {
            slot.0.entry(*k).or_insert_with(|| v.clone());
        }
        slot.1.extend_from_slice(payload);
        if slot.1.len() > MAX_B64 {
            pending.remove(&key);
        }
        return;
    }
    let (mut ctrl, mut data) = pending.remove(&key).unwrap_or_default();
    for (k, v) in &cur {
        ctrl.insert(*k, v.clone());
    }
    data.extend_from_slice(payload);
    let action = ctrl.get(&b'a').and_then(|s| s.bytes().next()).unwrap_or(b't');
    match action {
        b't' | b'T' | b'f' => {
            let Some((pw, ph, rgba)) = decode_image(&ctrl, &data, files) else {
                return;
            };
            let cols = num(&ctrl, b'c').filter(|v| *v > 0).map(|v| v as usize);
            let rows = num(&ctrl, b'r').filter(|v| *v > 0).map(|v| v as usize);
            stored.insert(key, Stored { rgba, pw, ph, cols, rows });
            if action == b't' {
                return;
            }
            display(term, proc, &ctrl, key, stored, placed, cell_w, cell_h);
        }
        b'p' => {
            display(term, proc, &ctrl, key, stored, placed, cell_w, cell_h);
        }
        b'd' => {
            match ctrl.get(&b'd').map(|s| s.as_str()).unwrap_or("a") {
                "a" | "A" => {
                    stored.clear();
                    pending.clear();
                    placed.clear();
                }
                "i" => {
                    stored.remove(&key);
                    pending.remove(&key);
                    placed.retain(|im| im.key != key);
                }
                "n" => {
                    let n = (num(&ctrl, b'I').unwrap_or(0).max(0) as u64) & 0xffff_ffff;
                    stored.retain(|k, _| k & 0xffff_ffff != n);
                    pending.retain(|k, _| k & 0xffff_ffff != n);
                    placed.retain(|im| im.key & 0xffff_ffff != n);
                }
                "p" => {
                    if let Some(p) = num(&ctrl, b'p') {
                        placed.retain(|im| im.pid != Some(p));
                    }
                }
                "c" => {
                    let cur = term.grid().cursor.point;
                    placed.retain(|im| !(im.line == cur.line.0 && im.col == cur.column.0 as usize));
                }
                _ => {
                    stored.clear();
                    pending.clear();
                    placed.clear();
                }
            }
        }
        _ => {}
    }
}

pub(crate) fn feed_with_kitty(
    term: &mut Term<VoidListener>,
    proc: &mut Processor,
    output: &[u8],
    files: &HashMap<Vec<u8>, Option<Vec<u8>>>,
    cell_w: u32,
    cell_h: u32,
) -> Vec<KittyImage> {
    let mut stored: HashMap<u64, Stored> = HashMap::new();
    let mut pending: HashMap<u64, (HashMap<u8, String>, Vec<u8>)> = HashMap::new();
    let mut placed: Vec<KittyImage> = Vec::new();
    let mut pos = 0usize;
    while pos < output.len() {
        let chunk_end = match find_esc_us(&output[pos..]) {
            None => {
                proc.advance(term, &output[pos..]);
                break;
            }
            Some(k) => pos + k,
        };
        if chunk_end + 2 >= output.len() || output[chunk_end + 2] != b'G' {
            let take = (chunk_end + 2).min(output.len());
            proc.advance(term, &output[pos..take]);
            pos = take;
            continue;
        }
        proc.advance(term, &output[pos..chunk_end]);
        match find_st(&output[chunk_end + 3..]) {
            None => break,
            Some((cmd_len, term_len)) => {
                let cmd = &output[chunk_end + 3..chunk_end + 3 + cmd_len];
                handle_kitty(
                    term, proc, cmd, &mut stored, &mut pending, &mut placed, files,
                    cell_w, cell_h,
                );
                pos = chunk_end + 3 + cmd_len + term_len;
            }
        }
    }
    placed
}

pub(crate) fn graphics_query_answers(new_bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = Vec::new();
    let mut seen: Vec<i64> = Vec::new();
    let mut pos = 0usize;
    while pos < new_bytes.len() && out.len() < 4 {
        let k = match find_esc_us(&new_bytes[pos..]) {
            None => break,
            Some(k) => pos + k,
        };
        if k + 2 >= new_bytes.len() || new_bytes[k + 2] != b'G' {
            pos = (k + 2).min(new_bytes.len());
            continue;
        }
        let (cmd_len, term_len) = match find_st(&new_bytes[k + 3..]) {
            None => break,
            Some(t) => t,
        };
        let cmd = &new_bytes[k + 3..k + 3 + cmd_len];
        let raw_ctrl = match cmd.iter().position(|&b| b == b';') {
            Some(s) => &cmd[..s],
            None => cmd,
        };
        let ctrl = parse_ctrl(raw_ctrl);
        if ctrl.get(&b'a').map(|s| s.as_str()) == Some("q") {
            let id = num(&ctrl, b'i').unwrap_or(1);
            if !seen.contains(&id) {
                seen.push(id);
                let mut r = vec![0x1b, b'_', b'G'];
                r.extend_from_slice(format!("i={id};OK").as_bytes());
                r.extend_from_slice(&[0x1b, b'\\']);
                out.push(r);
            }
        }
        pos = k + 3 + cmd_len + term_len;
    }
    out
}

pub(crate) fn needed_guest_files(output: &[u8]) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = Vec::new();
    let mut pos = 0usize;
    while pos < output.len() && out.len() < 8 {
        let k = match find_esc_us(&output[pos..]) {
            None => break,
            Some(k) => pos + k,
        };
        if k + 2 >= output.len() || output[k + 2] != b'G' {
            pos = (k + 2).min(output.len());
            continue;
        }
        let (cmd_len, term_len) = match find_st(&output[k + 3..]) {
            None => break,
            Some(t) => t,
        };
        let cmd = &output[k + 3..k + 3 + cmd_len];
        let (raw_ctrl, payload) = match cmd.iter().position(|&b| b == b';') {
            Some(s) => (&cmd[..s], &cmd[s + 1..]),
            None => (cmd, &[][..]),
        };
        let ctrl = parse_ctrl(raw_ctrl);
        let medium = ctrl.get(&b't').map(|s| s.as_str()).unwrap_or("d");
        if (medium == "f" || medium == "t")
            && !payload.is_empty()
            && payload.len() <= 4096
            && !payload.contains(&0)
            && !out.contains(&payload.to_vec())
        {
            out.push(payload.to_vec());
        }
        pos = k + 3 + cmd_len + term_len;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::termrender::{TermDims, TERM_ROWS};
    use alacritty_terminal::term::Config;

    fn test_term() -> (Term<VoidListener>, Processor) {
        let term: Term<VoidListener> = Term::new(
            Config { scrolling_history: TERM_ROWS, ..Default::default() },
            &TermDims,
            VoidListener,
        );
        (term, Processor::new())
    }

    fn apc(ctrl: &str, payload_b64: &str) -> Vec<u8> {
        let mut v = vec![0x1b, b'_', b'G'];
        v.extend_from_slice(ctrl.as_bytes());
        v.push(b';');
        v.extend_from_slice(payload_b64.as_bytes());
        v.extend_from_slice(&[0x1b, b'\\']);
        v
    }

    fn tiny_png_b64() -> String {
        use base64::Engine as _;
        let rgb = vec![255u8, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0];
        let png = crate::pngencode::encode_rgb(2, 2, &rgb).expect("encodes");
        base64::engine::general_purpose::STANDARD.encode(&png)
    }

    #[test]
    fn transmit_display_png_places_at_cursor() {
        let (mut term, mut proc) = test_term();
        let mut out = b"AB\nCD".to_vec();
        out.extend_from_slice(&apc("a=T,f=100,i=7,q=2,c=4,r=2", &tiny_png_b64()));
        let placed = feed_with_kitty(&mut term, &mut proc, &out, &HashMap::new(), 10, 20);
        assert_eq!(placed.len(), 1);
        let im = &placed[0];
        assert_eq!((im.pw, im.ph), (2, 2));
        assert_eq!((im.line, im.col), (1, 4));
        assert_eq!((im.cols, im.rows), (4, 2));
        assert_eq!(&im.rgba[0..4], &[255, 0, 0, 255]);
        assert_eq!(&im.rgba[4..8], &[0, 255, 0, 255]);
    }

    #[test]
    fn chunked_transmission_reassembles() {
        let b64 = tiny_png_b64();
        let (mut term, mut proc) = test_term();
        let mut out = Vec::new();
        out.extend_from_slice(&apc("a=T,f=100,i=3,q=2,m=1", &b64[..b64.len() / 2]));
        out.extend_from_slice(&apc("m=0", &b64[b64.len() / 2..]));
        let placed = feed_with_kitty(&mut term, &mut proc, &out, &HashMap::new(), 10, 20);
        assert_eq!(placed.len(), 1);
        assert_eq!((placed[0].pw, placed[0].ph), (2, 2));
    }

    #[test]
    fn transmit_then_place_by_id() {
        let (mut term, mut proc) = test_term();
        let mut out = Vec::new();
        out.extend_from_slice(&apc("a=t,f=100,i=9,q=2", &tiny_png_b64()));
        out.extend_from_slice(b"hello");
        out.extend_from_slice(&apc("a=p,i=9,c=2,r=1", ""));
        let placed = feed_with_kitty(&mut term, &mut proc, &out, &HashMap::new(), 10, 20);
        assert_eq!(placed.len(), 1);
        assert_eq!(placed[0].col, 5);
    }

    #[test]
    fn delete_all_clears_placements() {
        let (mut term, mut proc) = test_term();
        let mut out = Vec::new();
        out.extend_from_slice(&apc("a=T,f=100,i=1", &tiny_png_b64()));
        out.extend_from_slice(&apc("a=d,d=a", ""));
        let placed = feed_with_kitty(&mut term, &mut proc, &out, &HashMap::new(), 10, 20);
        assert!(placed.is_empty());
    }

    #[test]
    fn raw_rgb24_decodes() {
        use base64::Engine as _;
        let raw = vec![10u8, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120];
        let b64 = base64::engine::general_purpose::STANDARD.encode(&raw);
        let (mut term, mut proc) = test_term();
        let out = apc("a=T,f=24,s=2,v=2,i=5", &b64);
        let placed = feed_with_kitty(&mut term, &mut proc, &out, &HashMap::new(), 10, 20);
        assert_eq!(placed.len(), 1);
        assert_eq!(&placed[0].rgba[0..4], &[10, 20, 30, 255]);
        assert_eq!(&placed[0].rgba[12..16], &[100, 110, 120, 255]);
    }

    #[test]
    fn unknown_format_ignored_text_survives() {
        let (mut term, mut proc) = test_term();
        let mut out = b"hi".to_vec();
        out.extend_from_slice(&apc("a=T,f=999,i=1", "AAAA"));
        out.extend_from_slice(b"yo");
        let placed = feed_with_kitty(&mut term, &mut proc, &out, &HashMap::new(), 10, 20);
        assert!(placed.is_empty());
        let grid = term.grid();
        use alacritty_terminal::index::{Column, Line};
        assert_eq!(grid[Line(0)][Column(0)].c, 'h');
        assert_eq!(grid[Line(0)][Column(2)].c, 'y');
    }

    #[test]
    fn unterminated_sequence_never_reaches_grid() {
        let (mut term, mut proc) = test_term();
        let mut out = b"ok".to_vec();
        out.extend_from_slice(&[0x1b, b'_', b'G', b'a', b'=', b'T']);
        out.extend_from_slice(b"QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVph");
        let placed = feed_with_kitty(&mut term, &mut proc, &out, &HashMap::new(), 10, 20);
        assert!(placed.is_empty());
        let grid = term.grid();
        use alacritty_terminal::index::{Column, Line};
        assert_eq!(grid[Line(0)][Column(0)].c, 'o');
        assert_eq!(grid[Line(0)][Column(1)].c, 'k');
        assert_eq!(grid[Line(0)][Column(2)].c, ' ');
    }

    #[test]
    fn graphics_probe_gets_ok_answer() {
        let mut q = vec![0x1b, b'_', b'G'];
        q.extend_from_slice(b"i=31,a=q,t=d,f=24,s=1,v=1;AAAA");
        q.extend_from_slice(&[0x1b, b'\\']);
        let answers = graphics_query_answers(&q);
        assert_eq!(answers.len(), 1);
        assert_eq!(answers[0], vec![0x1b, b'_', b'G', b'i', b'=', b'3', b'1', b';', b'O', b'K', 0x1b, b'\\']);
        let mut q2 = q.clone();
        q2.extend_from_slice(&q);
        assert_eq!(graphics_query_answers(&q2).len(), 1);
        assert!(graphics_query_answers(b"plain text").is_empty());
        let mut t = vec![0x1b, b'_', b'G'];
        t.extend_from_slice(b"a=T,f=24,s=1,v=1;");
        t.extend_from_slice(&[0x1b, b'\\']);
        assert!(graphics_query_answers(&t).is_empty());
    }

    #[test]
    fn file_transfer_lists_guest_paths() {
        let mut out = Vec::new();
        out.extend_from_slice(&apc("a=T,f=100,t=f,i=4", "/tmp/pic.png"));
        out.extend_from_slice(&apc("a=T,f=100,i=5", "AAAA"));
        assert_eq!(needed_guest_files(&out), vec![b"/tmp/pic.png".to_vec()]);
        assert!(needed_guest_files(b"no graphics here").is_empty());
    }

    #[test]
    fn file_backed_display_renders() {
        let rgb = vec![1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        let png = crate::pngencode::encode_rgb(2, 2, &rgb).expect("encodes");
        let mut files: HashMap<Vec<u8>, Option<Vec<u8>>> = HashMap::new();
        files.insert(b"/g/pic.png".to_vec(), Some(png));
        let (mut term, mut proc) = test_term();
        let out = apc("a=T,f=100,t=f,i=2,c=3,r=3", "/g/pic.png");
        let placed = feed_with_kitty(&mut term, &mut proc, &out, &files, 10, 20);
        assert_eq!(placed.len(), 1);
        assert_eq!((placed[0].pw, placed[0].ph), (2, 2));
        assert_eq!(&placed[0].rgba[0..4], &[1, 2, 3, 255]);
        let (mut term2, mut proc2) = test_term();
        let out2 = apc("a=T,f=100,t=f,i=2", "/g/missing.png");
        let placed2 = feed_with_kitty(&mut term2, &mut proc2, &out2, &files, 10, 20);
        assert!(placed2.is_empty());
    }

    #[test]
    fn feed_matches_plain_emulation_on_ansi_soup() {
        let mut soup = Vec::new();
        soup.extend_from_slice(b"\x1b]0;test title\x07");
        soup.extend_from_slice("\x1b[0;32m$ jefetch --static\x1b[0m\n".as_bytes());
        soup.extend_from_slice("\x1b[38;5;196mred256\x1b[0m \x1b[38;2;1;2;3mrgb\x1b[0m\n".as_bytes());
        soup.extend_from_slice("▗▒▓▓▓▓▓▒▒▒▄▄░▒▒▒▓▒ CPU-> x\n".as_bytes());
        soup.extend_from_slice("┌─┐\t│tab│\n".as_bytes());
        soup.extend_from_slice(b"plain tail");
        let term_a = crate::termrender::emulate_output(&soup);
        let (mut term_b, mut proc_b) = test_term();
        let placed = feed_with_kitty(&mut term_b, &mut proc_b, &soup, &HashMap::new(), 10, 20);
        assert!(placed.is_empty());
        let (ga, gb) = (term_a.grid(), term_b.grid());
        use alacritty_terminal::index::{Column, Line};
        use crate::termrender::{resolve_color, TERM_COLS, TERM_ROWS};
        for row in 0..TERM_ROWS {
            for col in 0..TERM_COLS {
                let a = &ga[Line(row as i32)][Column(col)];
                let b = &gb[Line(row as i32)][Column(col)];
                assert_eq!(a.c, b.c, "char at {}:{}", row, col);
                assert_eq!(resolve_color(a.fg), resolve_color(b.fg), "fg at {}:{}", row, col);
                assert_eq!(resolve_color(a.bg), resolve_color(b.bg), "bg at {}:{}", row, col);
            }
        }
        assert_eq!(ga.cursor.point, gb.cursor.point);
    }

    #[test]
    fn png_decode_round_trips_encoder() {
        let mut rgb = vec![0u8; 5 * 4 * 3];
        for (k, px) in rgb.chunks_exact_mut(3).enumerate() {
            px[0] = ((k * 37) % 251) as u8;
            px[1] = ((k * 91) % 251) as u8;
            px[2] = ((k * 53) % 251) as u8;
        }
        let png = crate::pngencode::encode_rgb(5, 4, &rgb).expect("encodes");
        let (w, h, rgba) = crate::pngencode::decode_png_rgba(&png).expect("decodes");
        assert_eq!((w, h), (5, 4));
        for (k, px) in rgba.chunks_exact(4).enumerate() {
            assert_eq!(&rgb[k * 3..k * 3 + 3], &px[..3]);
            assert_eq!(px[3], 0xff);
        }
        assert!(crate::pngencode::decode_png_rgba(b"not a png").is_none());
        assert!(crate::pngencode::decode_png_rgba(&png[..30]).is_none());
    }
}
