use std::path::PathBuf;

use crate::scrub::utf8_len;

pub(crate) fn valid_runas(name: &str) -> bool {
    if name.is_empty() || name.len() > 32 || name == "root" {
        return false;
    }
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

pub(crate) fn urandom_bytes(n: usize) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut f = std::fs::File::open("/dev/urandom").ok()?;
    let mut buf = vec![0u8; n];
    f.read_exact(&mut buf).ok()?;
    Some(buf)
}

pub(crate) fn hex_bytes(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

pub(crate) fn random_suffix() -> String {
    if let Some(bytes) = urandom_bytes(8) {
        return hex_bytes(&bytes);
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}-{}", nanos, std::process::id())
}

pub(crate) fn sanitize_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut swatch: Option<i32> = None;
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\r' {
            continue;
        }
        if c == '\t' {
            swatch = None;
            out.push_str("        ");
            continue;
        }
        if c != '\x1b' {
            if c == ' ' {
                if let Some(fg) = swatch {
                    let mut n = 1;
                    while it.peek() == Some(&' ') {
                        it.next();
                        n += 1;
                    }
                    if n >= 2 {
                        out.push_str(&format!("\x1b[0;{}m", fg));
                        for _ in 0..n {
                            out.push('█');
                        }
                        out.push_str("\x1b[0m");
                    } else {
                        out.push(' ');
                    }
                    continue;
                }
            } else {
                swatch = None;
            }
            out.push(c);
            continue;
        }
        swatch = None;
        match it.peek() {
            Some('[') => {
                it.next();
                let mut params = String::new();
                let mut final_b = None;
                for ch in it.by_ref() {
                    if ('@'..='~').contains(&ch) {
                        final_b = Some(ch);
                        break;
                    }
                    params.push(ch);
                }
                if final_b == Some('m') {
                    let mut kept = vec![];
                    let mut bg = None;
                    for p in params.split(';') {
                        let n: i32 = p.parse().unwrap_or(-1);
                        match n {
                            0 | 1 | 4 | 22 | 24 | 39 | 49 => kept.push(n.to_string()),
                            30..=37 => kept.push(n.to_string()),
                            90..=97 => kept.push((n - 60).to_string()),
                            40..=47 => {
                                if bg.is_none() {
                                    bg = Some(n - 10);
                                }
                            }
                            100..=107 => {
                                if bg.is_none() {
                                    bg = Some(n - 70);
                                }
                            }
                            _ => {}
                        }
                    }
                    swatch = bg;
                    if !kept.is_empty() || params.is_empty() {
                        if kept.is_empty() {
                            out.push_str("\x1b[0m");
                        } else if kept.len() == 1 && kept[0] != "0" {
                            out.push_str("\x1b[0;");
                            out.push_str(&kept[0]);
                            out.push('m');
                        } else {
                            out.push_str("\x1b[");
                            out.push_str(&kept.join(";"));
                            out.push('m');
                        }
                    }
                }
            }
            Some(']') => {
                it.next();
                let mut prev = '\0';
                for ch in it.by_ref() {
                    if ch == '\x07' || (ch == '\\' && prev == '\x1b') {
                        break;
                    }
                    prev = ch;
                }
            }
            Some(_) => {
                it.next();
            }
            None => {}
        }
    }
    out
}

pub(crate) fn codeblock(s: &str) -> String {
    let mut t = sanitize_ansi(s.trim_end());
    if t.len() > 1800 {
        t = t.chars().take(1790).collect();
        t.push_str("\n…truncated");
    }
    if t.is_empty() {
        t = "(empty)".into();
    }
    format!("```\n{}\n```", t)
}

pub(crate) fn fit_bottom_lines(body: &str) -> (String, bool) {
    const MAX: usize = 1750;
    let lines: Vec<&str> = body.lines().collect();
    let mut kept: Vec<String> = Vec::new();
    let mut len = 0usize;
    let mut truncated = false;
    for l in lines.iter().rev() {
        if l.chars().count() + 1 > MAX {
            if kept.is_empty() {
                let v: Vec<char> = l.chars().collect();
                let start = v.len().saturating_sub(MAX - 1);
                let mut s: String = v[start..].iter().collect();
                s.push('\n');
                kept.push(s);
            }
            truncated = true;
            break;
        }
        let n = l.chars().count() + 1;
        if len + n > MAX {
            truncated = true;
            break;
        }
        kept.push(l.to_string());
        len += n;
    }
    kept.reverse();
    (kept.join("\n"), truncated)
}

pub(crate) fn plain_tail(body: &str) -> String {
    let cr = body.replace('\r', "\n");
    let clean = strip_sgr(after_last_clear(cr.trim_end()));
    let (fitted, truncated) = fit_bottom_lines(&clean);
    let t = if fitted.trim().is_empty() {
        "(empty)".to_string()
    } else {
        fitted
    };
    if truncated {
        format!("```\n…\n{}\n```", t)
    } else {
        format!("```\n{}\n```", t)
    }
}

pub(crate) const LIVE_IMG_MAX_COLS: usize = 120;
pub(crate) const LIVE_IMG_MAX_ROWS: usize = 80;
// 2x supersampled metrics (32px font): big and crisp in chat.
// Floor keeps tiny outputs from rendering as thumbnails.
const LIVE_IMG_COL_PX: u32 = 22;
const LIVE_IMG_ROW_PX: u32 = 48;
const LIVE_IMG_PAD_PX: u32 = 20;
const LIVE_IMG_MIN_W: u32 = 640;
const LIVE_IMG_MIN_H: u32 = 400;

/// Byte spans (start, end) of terminal clear-screen sequences: CSI J
/// (erase display), CSI H/f (cursor home/position), ESC c (full reset).
/// The spans are ASCII-only, so both ends are always char boundaries.
fn clear_cuts(s: &str) -> Vec<(usize, usize)> {
    let b = s.as_bytes();
    let mut cuts = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == 0x1b && i + 1 < b.len() {
            if b[i + 1] == b'c' {
                cuts.push((i, i + 2));
                i += 2;
                continue;
            }
            if b[i + 1] == b'[' {
                let mut j = i + 2;
                while j < b.len() && (b[j].is_ascii_digit() || b[j] == b';' || b[j] == b'?') {
                    j += 1;
                }
                if j < b.len() && (b[j] == b'J' || b[j] == b'H' || b[j] == b'f') {
                    cuts.push((i, j + 1));
                    i = j + 1;
                    continue;
                }
            }
        }
        i = (i + utf8_len(b[i])).min(b.len());
    }
    cuts
}

/// Terminal emulators replace the screen on clear/home sequences instead of
/// appending. Return everything after the last such sequence so animated
/// redraws (clear + redraw loops) show the current frame, not stacked history.
/// Must run on the raw text, before strip_sgr removes the sequences.
pub(crate) fn after_last_clear(s: &str) -> &str {
    match clear_cuts(s).last() {
        Some(&(_, end)) => &s[end..],
        None => s,
    }
}

/// Like after_last_clear, but if the current frame is blank (a poll landed
/// right after a clear while the program is still redrawing), fall back to
/// the newest non-blank frame instead of flashing empty. Returned slices
/// never contain a clear sequence, so feeding them through after_last_clear
/// again is a safe no-op. For finished output prefer after_last_clear (a
/// trailing clear there is the genuine final state).
pub(crate) fn current_frame(s: &str) -> &str {
    let cuts = clear_cuts(s);
    // Content runs between the clear sequences (seq bytes excluded).
    let mut segs: Vec<(usize, usize)> = Vec::with_capacity(cuts.len() + 1);
    let mut start = 0;
    for &(st, en) in &cuts {
        segs.push((start, st));
        start = en;
    }
    segs.push((start, s.len()));
    for &(a, b) in segs.iter().rev() {
        if !s[a..b].trim().is_empty() {
            return &s[a..b];
        }
    }
    ""
}

/// Prepare terminal output for image rendering: turn carriage returns into
/// newlines (so progress-bar redraws become lines instead of glued text),
/// strip colors, expand tabs, keep the last MAX_ROWS lines, truncate lines
/// to MAX_COLS chars.
/// Returns (renderable text, image width, image height) sized to the text.
pub(crate) fn frame_text(body: &str) -> (String, u32, u32) {
    let cr = body.replace('\r', "\n");
    let clean = strip_sgr(after_last_clear(&cr));
    let lines: Vec<&str> = clean.lines().collect();
    let start = lines.len().saturating_sub(LIVE_IMG_MAX_ROWS);
    let mut out: Vec<String> = Vec::new();
    let mut cols: usize = 0;
    for l in &lines[start..] {
        let expanded: Vec<char> = l.replace('\t', "        ").chars().collect();
        let len = expanded.len().min(LIVE_IMG_MAX_COLS);
        cols = cols.max(len);
        out.push(expanded[..len].iter().collect());
    }
    if out.is_empty() {
        out.push("(empty)".to_string());
        cols = cols.max(7);
    }
    let w = (cols as u32 * LIVE_IMG_COL_PX + LIVE_IMG_PAD_PX * 2).max(LIVE_IMG_MIN_W);
    let h = (out.len() as u32 * LIVE_IMG_ROW_PX + LIVE_IMG_PAD_PX * 2).max(LIVE_IMG_MIN_H);
    (out.join("\n"), w, h)
}

pub(crate) fn cap_file_body(clean: &str) -> String {
    const FILE_MAX: usize = 400_000;
    if clean.chars().count() <= FILE_MAX {
        return clean.to_string();
    }
    let v: Vec<char> = clean.chars().collect();
    let start = v.len() - FILE_MAX;
    format!(
        "…[showing last {} chars]\n{}",
        FILE_MAX,
        v[start..].iter().collect::<String>()
    )
}
pub(crate) fn attach_name(cmd: &str) -> String {
    let w: String = cmd
        .split_whitespace()
        .next()
        .unwrap_or("output")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if w.is_empty() {
        "output.txt".into()
    } else {
        format!("{}.txt", w)
    }
}

/// Locate a helper binary without relying on PATH (systemd services run with
/// a minimal PATH that often lacks ffmpeg/fontconfig on NixOS).
pub(crate) fn tool_path(name: &str) -> Option<std::path::PathBuf> {
    if name.is_empty() || name.contains('/') {
        return None;
    }
    let mut dirs: Vec<std::path::PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    dirs.push("/run/current-system/sw/bin".into());
    dirs.push("/run/wrappers/bin".into());
    if let Some(home) = std::env::var_os("HOME") {
        let home = std::path::PathBuf::from(home);
        dirs.push(home.join(".nix-profile/bin"));
        if let Ok(rd) = std::fs::read_dir("/etc/profiles/per-user") {
            for e in rd.flatten() {
                dirs.push(e.path().join("bin"));
            }
        }
    }
    dirs.push("/nix/profile/bin".into());
    dirs.push("/usr/local/bin".into());
    dirs.push("/usr/bin".into());
    dirs.push("/bin".into());
    for d in &dirs {
        let p = d.join(name);
        if is_executable(&p) {
            return Some(p);
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(p: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.is_file()
        && p.metadata()
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(p: &std::path::Path) -> bool {
    p.is_file()
}


pub(crate) fn strip_sgr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == 0x1b && i + 1 < b.len() && b[i + 1] == b'[' {
            let mut j = i + 2;
            while j < b.len() && !b[j].is_ascii_alphabetic() {
                j += 1;
            }
            i = (j + 1).min(b.len());
            continue;
        }
        if b[i] == 0x1b {
            if i + 1 < b.len() && b[i + 1] == b']' {
                let mut j = i + 2;
                while j < b.len() && b[j] != 0x07 {
                    if b[j] == 0x1b && j + 1 < b.len() && b[j + 1] == b'\\' {
                        j += 2;
                        break;
                    }
                    j += 1;
                }
                if j < b.len() && b[j] == 0x07 {
                    j += 1;
                }
                i = j;
                continue;
            }
            i += 1;
            if i < b.len() {
                i += 1;
            }
            continue;
        }
        if b[i] == b'\r' {
            i += 1;
            continue;
        }
        let len = utf8_len(b[i]);
        let end = (i + len).min(b.len());
        out.push_str(&s[i..end]);
        i = end;
    }
    out
}

pub(crate) fn deployed_via_nix() -> bool {
    std::env::current_exe()
        .map(|p| p.starts_with("/nix/store/"))
        .unwrap_or(false)
}

pub(crate) fn project_dir() -> PathBuf {
    let mut dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()));
    for _ in 0..5 {
        match dir {
            Some(ref p) if p.join("Cargo.toml").exists() => return p.clone(),
            Some(p) => dir = p.parent().map(|p| p.to_path_buf()),
            None => break,
        }
    }
    PathBuf::from(".")
}

