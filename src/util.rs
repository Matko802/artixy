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
    format!("```ansi\n{}\n```", t)
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

pub(crate) fn fence_inline(fitted: &str) -> String {
    let t = if fitted.trim().is_empty() {
        "(empty)".to_string()
    } else {
        fitted.to_string()
    };
    format!("```ansi\n{}\n```", t)
}

pub(crate) fn plain_tail(body: &str) -> String {
    let clean = strip_sgr(body.trim_end());
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

