pub(crate) fn public_ipv4(o: [u8; 4]) -> bool {
    match o {
        [10, _, _, _] => false,
        [172, b, _, _] if (16..=31).contains(&b) => false,
        [192, 168, _, _] => false,
        [127, _, _, _] => false,
        [169, 254, _, _] => false,
        [100, b, _, _] if (64..=127).contains(&b) => false,
        [0, _, _, _] => false,
        [255, 255, 255, 255] => false,
        _ => true,
    }
}

pub(crate) fn scan_ipv4(b: &[u8], i: usize) -> Option<([u8; 4], usize)> {
    if i > 0 {
        let p = b[i - 1];
        if p.is_ascii_alphanumeric() || p == b'.' {
            return None;
        }
    }
    let mut o = [0u8; 4];
    let mut j = i;
    for k in 0..4 {
        let start = j;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        let len = j - start;
        if len == 0 || len > 3 {
            return None;
        }
        let mut v: u16 = 0;
        for d in &b[start..j] {
            v = v * 10 + (d - b'0') as u16;
        }
        if v > 255 {
            return None;
        }
        o[k] = v as u8;
        if k < 3 {
            if j >= b.len() || b[j] != b'.' {
                return None;
            }
            j += 1;
        }
    }
    match b.get(j) {
        Some(d) if d.is_ascii_digit() => return None,
        Some(b'.') => {
            if matches!(b.get(j + 1), Some(d) if d.is_ascii_digit()) {
                return None;
            }
        }
        _ => {}
    }
    Some((o, j))
}

pub(crate) fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

pub(crate) fn parse_quad(s: &str) -> Option<[u8; 4]> {
    let p: Vec<&str> = s.split('.').collect();
    if p.len() != 4 {
        return None;
    }
    let mut o = [0u8; 4];
    for (k, g) in p.iter().enumerate() {
        if g.is_empty() || g.len() > 3 {
            return None;
        }
        let mut v: u16 = 0;
        for d in g.bytes() {
            if !d.is_ascii_digit() {
                return None;
            }
            v = v * 10 + (d - b'0') as u16;
        }
        if v > 255 {
            return None;
        }
        o[k] = v as u8;
    }
    Some(o)
}

pub(crate) fn valid_groups(parts: &[&str]) -> bool {
    parts
        .iter()
        .all(|p| !p.is_empty() && p.len() <= 4 && p.bytes().all(|c| c.is_ascii_hexdigit()))
}

pub(crate) fn classify_ipv6(tok: &str) -> Option<bool> {
    let (head, tail) = if tok.contains('.') {
        let cut = tok.rfind(':')?;
        (tok[..cut].to_string(), Some(parse_quad(&tok[cut + 1..])?))
    } else {
        (tok.to_string(), None)
    };
    if head.matches("::").count() > 1 {
        return None;
    }
    let has_dbl = head.contains("::");
    let need = if tail.is_some() { 2 } else { 0 };
    let explicit: Vec<&str> = if has_dbl {
        let p = head.find("::").unwrap_or(0);
        let l: Vec<&str> = if head[..p].is_empty() {
            Vec::new()
        } else {
            head[..p].split(':').collect()
        };
        let r: Vec<&str> = if head[p + 2..].is_empty() {
            Vec::new()
        } else {
            head[p + 2..].split(':').collect()
        };
        if !valid_groups(&l) || !valid_groups(&r) {
            return None;
        }
        if l.len() + r.len() + need > 7 {
            return None;
        }
        l.into_iter().chain(r).collect()
    } else {
        let g: Vec<&str> = head.split(':').collect();
        if !valid_groups(&g) || g.len() + need != 8 {
            return None;
        }
        g
    };
    let vals: Vec<u16> = explicit
        .iter()
        .map(|g| u16::from_str_radix(g, 16).unwrap_or(0))
        .collect();
    if vals.iter().all(|v| *v == 0) {
        return match tail {
            Some(q) => Some(public_ipv4(q)),
            None => Some(false),
        };
    }
    let loopback = if has_dbl {
        vals.iter().skip_while(|v| **v == 0).copied().collect::<Vec<u16>>() == [1]
    } else {
        vals.len() == 8 && vals[..7].iter().all(|v| *v == 0) && vals[7] == 1
    };
    if loopback {
        return Some(false);
    }
    let g0 = vals[0];
    if (0xfe80..=0xfebf).contains(&g0) || (0xfc00..=0xfdff).contains(&g0) {
        return Some(false);
    }
    Some(true)
}

pub(crate) fn scan_ipv6(s: &str, i: usize) -> Option<(usize, bool)> {
    let b = s.as_bytes();
    let c = b[i];
    if !(c.is_ascii_hexdigit() || c == b':') {
        return None;
    }
    if i > 0 {
        let p = b[i - 1];
        if p.is_ascii_hexdigit() || p == b':' || p == b'.' {
            return None;
        }
    }
    let mut j = i;
    let mut colons = 0usize;
    while j < b.len() && (b[j].is_ascii_hexdigit() || b[j] == b':' || b[j] == b'.') {
        if b[j] == b':' {
            colons += 1;
        }
        j += 1;
    }
    if colons < 2 {
        return None;
    }
    let mut tok = &s[i..j];
    while tok.ends_with(':') && !tok.ends_with("::") {
        tok = &tok[..tok.len() - 1];
        j -= 1;
    }
    let public = classify_ipv6(tok)?;
    Some((j, public))
}

pub(crate) fn scrub_public_ip(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_hexdigit() || c == b':' {
            if let Some((end, public)) = scan_ipv6(s, i) {
                if public {
                    out.push_str("[redacted]");
                } else {
                    out.push_str(&s[i..end]);
                }
                i = end;
                continue;
            }
        }
        if c.is_ascii_digit() {
            if let Some((o, end)) = scan_ipv4(b, i) {
                if public_ipv4(o) {
                    out.push_str("[redacted]");
                } else {
                    out.push_str(&s[i..end]);
                }
                i = end;
                continue;
            }
        }
        let len = utf8_len(b[i]);
        out.push_str(&s[i..i + len]);
        i += len;
    }
    out
}

