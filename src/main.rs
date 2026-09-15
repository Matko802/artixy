mod ai;
mod commands;
mod config;
mod events;
mod kitty;
mod live;
mod pngencode;
mod scrub;
mod termrender;
mod util;
mod vm;
mod webhook;

use poise::serenity_prelude as serenity;

use crate::commands::{
    ai, botrestart, help, info, notify, ps, purge_replies, restart, run, sayas, send, shell,
    admin, start, status, stop, upload, user, warmode, websearch,
};
use crate::commands::BOOT_ART;
use crate::config::{Allowed, AllowedFile, BotSettings, Data, apply_legacy_import, config_file_path, ensure_config_template, load_file_config, save_json};
use crate::util::project_dir;

pub(crate) type Error = Box<dyn std::error::Error + Send + Sync>;
pub(crate) type Context<'a> = poise::Context<'a, Data, Error>;


#[cfg(test)]
mod tests {
    use crate::{commands::*, scrub::*, util::*};

    #[test]
    fn slash_commands_allow_user_install_everywhere() {
        let cmds = vec![
            help(),
            ps(),
            status(),
            start(),
            stop(),
            restart(),
            info(),
            user(),
            admin(),
            shell(),
            botrestart(),
            run(),
            sayas(),
            send(),
            notify(),
            purge_replies(),
            warmode(),
            upload(),
            ai(),
            websearch(),
        ];
        assert_eq!(cmds.len(), 20, "test must mirror the framework command list");
        for cmd in &cmds {
            let builder = cmd
                .create_as_slash_command()
                .unwrap_or_else(|| panic!("{} has no slash action", cmd.name));
            let v = serde_json::to_value(&builder).expect("serializes");
            assert_eq!(
                v.get("integration_types"),
                Some(&serde_json::json!([0, 1])),
                "{} must allow guild+user installs",
                cmd.name
            );
            assert_eq!(
                v.get("contexts"),
                Some(&serde_json::json!([0, 1, 2])),
                "{} must allow guild+DM contexts",
                cmd.name
            );
        }
    }

    #[test]
    fn palette_becomes_blocks() {
        let out = sanitize_ansi("\x1b[40m   \x1b[41m   \x1b[m");
        assert!(out.contains("\x1b[0;30m███\x1b[0m"), "got {:?}", out);
        assert!(out.contains("\x1b[0;31m███\x1b[0m"), "got {:?}", out);
        assert!(!out.contains("40m"), "got {:?}", out);
    }

    #[test]
    fn plain_tail_strips_colors_and_fits() {
        let out = plain_tail("\x1b[0;32m$ cmd\x1b[0m\nok");
        assert!(!out.contains('\x1b'), "no escapes, got {:?}", out);
        assert!(out.starts_with("```\n"), "plain fence, got {:?}", out);
        assert!(out.contains("$ cmd"), "content kept");
        assert!(!out.contains('…'), "no marker when nothing dropped");
        assert_eq!(plain_tail(""), "```\n(empty)\n```");
    }

    #[test]
    fn plain_tail_truncates_long_output() {
        let lines: Vec<String> = (0..30).map(|i| format!("line {:02} {}", i, "x".repeat(70))).collect();
        let body = lines.join("\n");
        assert!(body.chars().count() > 2000);
        let out = plain_tail(&body);
        assert!(out.chars().count() <= 2000, "fits Discord limit, got {}", out.chars().count());
        assert!(out.contains("line 29"), "bottom kept");
        assert!(!out.contains("line 00"), "head dropped");
        assert!(out.contains('…'), "truncation marked");
    }

    #[test]
    fn plain_tail_counts_chars_not_bytes() {
        let line = "▗▒▓▓▓▓▓▒▒▒▄▄░▒▒▒▓▒ CPU-> AMD Ryzen 5 5600G (2) @ 3.89 GHz";
        let body = (0..60).map(|i| format!("{} {}", line, i)).collect::<Vec<_>>().join("\n");
        let out = plain_tail(&body);
        assert!(out.chars().count() <= 2000, "fits Discord limit, got {}", out.chars().count());
        assert!(out.contains(" 59"), "bottom kept");
        assert!(!out.contains(" 00\n") && !out.starts_with("```\n▗▒▓▓▓▓▓▒▒▒▄▄░▒▒▒▓▒ CPU-> AMD Ryzen 5 5600G (2) @ 3.89 GHz 00"), "head dropped");
        assert!(out.contains('…'), "truncation marked");
    }

    #[test]
    fn after_last_clear_keeps_current_frame() {
        assert_eq!(after_last_clear("a\nb"), "a\nb");
        assert_eq!(after_last_clear("old\n\x1b[2Jnew"), "new");
        assert_eq!(after_last_clear("old\n\x1b[Hnew"), "new");
        assert_eq!(after_last_clear("old\n\x1bcnew"), "new");
        assert_eq!(after_last_clear("one\x1b[Jtwo\x1b[2Jthree"), "three", "last clear wins");
        assert_eq!(
            after_last_clear("\x1b[0;32mok\x1b[0m"),
            "\x1b[0;32mok\x1b[0m",
            "color sequences kept for the strip step"
        );
        assert_eq!(
            after_last_clear("a\x1b[?25lb"),
            "a\x1b[?25lb",
            "non-clear sequences ignored"
        );
        assert_eq!(after_last_clear(""), "");
    }

    #[test]
    fn build_runner_wraps_pty_matching_render_window() {
        let s = crate::live::build_runner("bash", "QkI2NA==", "/tmp/o.out", "/tmp/o.code", "/tmp/o.in", Some("matko802"));
        assert!(s.contains("stty cols 120 rows 40"), "pty matches render window");
        assert!(s.contains("TERM=xterm-256color"), "terminfo set");
        assert!(s.contains("CMD_DATA"), "command travels via env, not text");
        assert!(s.contains("script -qec"), "pty path first");
        assert!(s.contains("else bash -c"), "plain fallback");
        assert!(s.contains("/dev/null <> /tmp/o.in"), "script relays the fifo");
        assert!(!s.contains("</dev/null"), "app stdin is the pty, not null");
        assert!(s.contains("> /tmp/o.out 2>&1"), "output captured");
        assert!(s.contains("echo $? > /tmp/o.code"), "exit code kept");
        assert!(!s.contains("$(cat)"), "no pipe-through-pty (EOF would hang)");
        assert!(s.contains("cd ~matko802"), "starts in the user's home, not the daemon cwd");
        assert!(s.contains("cd /tmp"), "writable fallback so git/makepkg never land in /etc/dinit.d");
        assert!(s.contains("TERM_PROGRAM=rustyterm"), "fetch tools report rustyterm");
        assert!(s.contains("COLORTERM=truecolor"), "truecolor advertised");
        let sh = crate::live::build_runner("sh", "QkI2NA==", "/tmp/o.out", "/tmp/o.code", "/dev/null", None);
        assert!(sh.contains("else sh -c"), "sh fallback mirrors bash");
        assert!(sh.contains("<> /dev/null"), "null input stays EOF-fast");
        assert!(sh.contains("cd \"$HOME\""), "no-runas still leaves the daemon cwd");
        eprintln!("RUNNER=<<{}>>", s);
    }

    #[test]
    fn normalize_nl_collapses_crlf_first() {
        assert_eq!(crate::util::normalize_nl("a\r\nb"), "a\nb", "no doubling");
        assert_eq!(crate::util::normalize_nl("a\rb"), "a\nb", "lone CR");
        assert_eq!(crate::util::normalize_nl("a\nb"), "a\nb", "LF untouched");
    }

    #[test]
    fn terminal_emulator_feeds_text_and_colors() {
        use alacritty_terminal::{
            index::{Column, Line},
            vte::ansi::{Color, NamedColor},
        };
        use crate::termrender::*;
        let term = emulate_output(b"hello");
        let grid = term.grid();
        assert_eq!(grid[Line(0)][Column(0)].c, 'h');
        assert_eq!(grid[Line(0)][Column(4)].c, 'o');
        assert_eq!(grid[Line(0)][Column(5)].c, ' ');
        let term = emulate_output(b"\x1b[31mR\x1b[0mN");
        let grid = term.grid();
        assert_eq!(grid[Line(0)][Column(0)].c, 'R');
        assert_eq!(grid[Line(0)][Column(0)].fg, Color::Named(NamedColor::Red));
        assert_eq!(grid[Line(0)][Column(1)].c, 'N');
        assert_eq!(grid[Line(0)][Column(1)].fg, Color::Named(NamedColor::Foreground));
        let term = emulate_output(b"stale\n\x1b[2J\x1b[Hfresh");
        let grid = term.grid();
        assert_eq!(grid[Line(0)][Column(0)].c, 'f');
        let row: String = (0..5).map(|c| grid[Line(0)][Column(c)].c).collect();
        assert_eq!(row, "fresh");
    }

    #[test]
    fn terminal_viewport_shows_latest_after_scroll() {
        use alacritty_terminal::index::{Column, Line};
        use crate::termrender::*;
        let body: String = (0..100).map(|i| format!("line {:03}\r\n", i)).collect();
        let term = emulate_output(body.as_bytes());
        let grid = term.grid();
        let top: String = (0..8).map(|c| grid[Line(0)][Column(c)].c).collect();
        let bottom: String = (0..8).map(|c| grid[Line(39)][Column(c)].c).collect();
        eprintln!("top={:?} bottom={:?}", top, bottom);
        assert_eq!(top, "line 061", "viewport top follows scroll");
        assert_eq!(bottom, "        ", "cursor row is blank");
    }

    #[test]
    fn terminal_colors_resolve_sanely() {
        use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};
        use crate::termrender::*;
        assert_eq!(resolve_color(Color::Named(NamedColor::Red)), [0xcd, 0x00, 0x00]);
        assert_eq!(resolve_color(Color::Named(NamedColor::Foreground)), [0xe6, 0xe6, 0xe6]);
        assert_eq!(resolve_color(Color::Named(NamedColor::Background)), [0x0b, 0x0e, 0x14]);
        assert_eq!(resolve_color(Color::Spec(Rgb { r: 1, g: 2, b: 3 })), [1, 2, 3]);
        assert_eq!(indexed_color(0), [0x00, 0x00, 0x00]);
        assert_eq!(indexed_color(196), [0xff, 0x00, 0x00]);
        assert_eq!(indexed_color(231), [0xff, 0xff, 0xff]);
        assert_eq!(indexed_color(232), [8, 8, 8]);
        assert_eq!(dim([0xff, 0x30, 0x0c]), [0xaa, 0x20, 0x08]);
    }

    #[test]
    #[ignore]
    fn terminal_renders_real_jefetch_bytes() {
        use crate::termrender::*;
        let raw = std::fs::read("/tmp/termproof.bin").expect("capture first");
        let reg = crate::termrender::system_font_bytes("DejaVu Sans Mono").expect("regular font");
        let bold =
            crate::termrender::system_font_bytes("DejaVu Sans Mono:weight=bold").unwrap_or_else(|| reg.clone());
        let fonts = TermFonts::load(&reg, &bold).expect("fonts parse");
        let mut region = None;
        let png = render_terminal(&fonts, &raw, &std::collections::HashMap::new(), &mut region).expect("renders");
        std::fs::write("/tmp/termproof.png", &png).unwrap();
        eprintln!("rendered {} bytes, region {:?}", png.len(), region);
        assert!(png.len() > 20_000, "a real frame is not tiny");
    }

    #[test]
    fn terminal_content_region_skips_empty_edges() {
        use crate::termrender::*;
        assert_eq!(content_region(&emulate_output(b"hi")), (0, 1, 2));
        assert_eq!(content_region(&emulate_output(b"")), (0, 0, 0));
        assert_eq!(
            content_region(&emulate_output(b"\r\n\r\nab\r\ncde\r\n\r\n\r\n")),
            (2, 2, 3),
            "blank edges are outside the box"
        );
        let term = emulate_output(b"\x1b[41m   \x1b[0m\r\nplain\r\n\r\n\r\n");
        assert_eq!(content_region(&term), (0, 2, 5));
    }

    #[test]
    fn terminal_quantize_floors_buckets_and_locks() {
        use crate::termrender::*;
        assert_eq!(quantize_region(0, 0, None), (60, 12), "floor");
        assert_eq!(quantize_region(5, 3, None), (60, 12), "below floor");
        assert_eq!(quantize_region(21, 9, None), (60, 16), "floor beats small buckets");
        assert_eq!(quantize_region(120, 40, None), (120, 40), "ceiling");
        assert_eq!(quantize_region(999, 999, None), (120, 40), "clamped");
        assert_eq!(
            quantize_region(5, 3, Some((100, 32))),
            (100, 32),
            "lock wins upward"
        );
        assert_eq!(
            quantize_region(110, 40, Some((60, 12))),
            (120, 40),
            "content still grows the lock"
        );
    }

    #[test]
    fn kill_tree_script_targets_only_given_pid() {
        let s = crate::vm::kill_tree_script(1234);
        assert!(s.contains("pgrep -P"), "walks children, no name patterns");
        assert!(s.contains("killtree 1234"), "rooted at the pid");
        assert!(!s.contains("pkill"), "never pattern-kills by name");
        assert!(!s.contains("killall"), "never pattern-kills by name");
    }

    #[test]
    fn mkfifo_script_owns_fifo_for_the_reader() {
        let s = crate::live::mkfifo_script("/tmp/x.in", Some("matko802"));
        assert!(s.contains("mkfifo -m 600 /tmp/x.in"), "tight perms");
        assert!(s.contains("chown matko802 /tmp/x.in"), "reader must own it for <>");
        let s = crate::live::mkfifo_script("/tmp/x.in", None);
        assert!(!s.contains("chown"), "nothing to chown without runas");
    }

    #[test]
    fn pending_queries_fires_once_in_order() {
        use crate::live::pending_queries;
        assert!(pending_queries("plain output", false, false).is_empty());
        assert_eq!(pending_queries("\x1b[?u", false, false), vec!["\x1b[?0u"]);
        assert_eq!(pending_queries("\x1b[c", false, false), vec!["\x1b[?1;2c"]);
        assert!(pending_queries("\x1b[?u", true, false).is_empty(), "answered: silent");
        assert!(pending_queries("\x1b[c", false, true).is_empty(), "answered: silent");
        assert_eq!(
            pending_queries("x\x1b[c...\x1b[?u", false, false),
            vec!["\x1b[?1;2c", "\x1b[?0u"],
            "asked order preserved"
        );
        assert!(
            pending_queries("\x1b[?25l\x1b[38;5;1m\x1b[?1000h", false, false).is_empty(),
            "lookalikes (cursor-hide, colors, mouse) must not fire"
        );
    }

    #[test]
    fn png_crc32_matches_known_vectors() {
        use crate::pngencode::crc32;
        assert_eq!(crc32(b""), 0x00000000);
        assert_eq!(crc32(b"a"), 0xe8b7be43);
        assert_eq!(crc32(b"123456789"), 0xcbf43926);
        assert_eq!(
            crc32(b"The quick brown fox jumps over the lazy dog"),
            0x414fa339
        );
    }

    #[test]
    fn png_encode_rejects_bad_dims() {
        use crate::pngencode::encode_rgb;
        assert!(encode_rgb(0, 10, &[]).is_none());
        assert!(encode_rgb(10, 0, &[]).is_none());
        assert!(encode_rgb(2, 2, &[0u8; 11]).is_none());
        assert!(encode_rgb(2, 2, &[0u8; 13]).is_none());
        assert!(encode_rgb(1, 1, &[9u8, 9u8, 9u8]).is_some());
    }

    fn png_parse_chunks(png: &[u8]) -> Vec<(String, Vec<u8>)> {
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "signature");
        let mut chunks = Vec::new();
        let mut i = 8;
        while i + 8 <= png.len() {
            let len =
                u32::from_be_bytes([png[i], png[i + 1], png[i + 2], png[i + 3]]) as usize;
            let kind =
                String::from_utf8_lossy(&png[i + 4..i + 8]).into_owned();
            let data = png[i + 8..i + 8 + len].to_vec();
            let want = u32::from_be_bytes([
                png[i + 8 + len],
                png[i + 8 + len + 1],
                png[i + 8 + len + 2],
                png[i + 8 + len + 3],
            ]);
            let mut c = Vec::from(&png[i + 4..i + 8]);
            c.extend_from_slice(&data);
            assert_eq!(
                crate::pngencode::crc32(&c),
                want,
                "chunk {} CRC must verify",
                kind
            );
            chunks.push((kind, data));
            i += 12 + len;
        }
        chunks
    }

    fn png_pattern_rgb(w: u32, h: u32) -> Vec<u8> {
        let mut rgb = vec![0u8; (w * h * 3) as usize];
        for (i, px) in rgb.chunks_exact_mut(3).enumerate() {
            px[0] = ((i * 73 + 11) % 251) as u8;
            px[1] = ((i * 149 + 67) % 251) as u8;
            px[2] = ((i * 211 + 131) % 251) as u8;
        }
        rgb
    }

    fn png_assert_round_trip(png: &[u8], w: u32, h: u32, rgb: &[u8]) {
        let chunks = png_parse_chunks(png);
        let kinds: Vec<&str> = chunks.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(kinds.first().copied(), Some("IHDR"));
        assert_eq!(kinds.last().copied(), Some("IEND"));
        let ihdr = &chunks[0].1;
        assert_eq!(&ihdr[0..4], &w.to_be_bytes(), "width");
        assert_eq!(&ihdr[4..8], &h.to_be_bytes(), "height");
        assert_eq!((ihdr[8], ihdr[9], ihdr[10], ihdr[11]), (8, 2, 0, 0));
        let mut zlib: Vec<u8> = Vec::new();
        for (k, d) in &chunks {
            if k == "IDAT" {
                zlib.extend_from_slice(d);
            }
        }
        let raw = miniz_oxide::inflate::decompress_to_vec_zlib(&zlib).expect("inflates");
        assert_eq!(raw.len(), h as usize * (1 + w as usize * 3));
        for (row, stripe) in raw.chunks_exact(1 + w as usize * 3).enumerate() {
            assert_eq!(stripe[0], 0, "filter byte row {}", row);
            let s = row * w as usize * 3;
            assert_eq!(&stripe[1..], &rgb[s..s + w as usize * 3], "row {} pixels", row);
        }
    }

    #[test]
    fn png_encode_round_trip_through_miniz() {
        use crate::pngencode::{encode_rgb, encode_rgb_stored};
        let (w, h) = (300u32, 300u32);
        let rgb = png_pattern_rgb(w, h);
        png_assert_round_trip(&encode_rgb(w, h, &rgb).expect("encodes"), w, h, &rgb);
        png_assert_round_trip(
            &encode_rgb_stored(w, h, &rgb).expect("stored encodes"),
            w,
            h,
            &rgb,
        );
    }

    #[test]
    fn brighten_maps_normal_colors_to_bright() {
        use alacritty_terminal::vte::ansi::{Color, NamedColor};
        use crate::termrender::*;
        let pairs = [
            (NamedColor::Black, NamedColor::BrightBlack),
            (NamedColor::Red, NamedColor::BrightRed),
            (NamedColor::Green, NamedColor::BrightGreen),
            (NamedColor::Yellow, NamedColor::BrightYellow),
            (NamedColor::Blue, NamedColor::BrightBlue),
            (NamedColor::Magenta, NamedColor::BrightMagenta),
            (NamedColor::Cyan, NamedColor::BrightCyan),
            (NamedColor::White, NamedColor::BrightWhite),
        ];
        for (plain, bright) in pairs {
            assert_eq!(brighten(Color::Named(plain)), Color::Named(bright));
            assert_eq!(resolve_color(brighten(Color::Named(plain))), resolve_color(Color::Named(bright)));
        }
        assert_eq!(resolve_color(brighten(Color::Named(NamedColor::Cyan))), [0x00, 0xff, 0xff]);
        for passthrough in [
            Color::Named(NamedColor::BrightRed),
            Color::Named(NamedColor::Foreground),
            Color::Named(NamedColor::Background),
            Color::Indexed(196),
        ] {
            assert_eq!(brighten(passthrough), passthrough);
        }
    }

    #[test]
    fn terminal_key_maps_hotkeys_to_bytes() {
        use crate::live::terminal_key;
        assert_eq!(terminal_key(";return"), Some("\x7f".to_string()));
        assert_eq!(terminal_key(";space"), Some(" ".to_string()));
        assert_eq!(terminal_key(";enter"), Some("\r".to_string()));
        assert_eq!(terminal_key(";esc"), Some("\x1b".to_string()));
        assert_eq!(terminal_key(";up"), Some("\x1b[A".to_string()));
        assert_eq!(terminal_key(";down"), Some("\x1b[B".to_string()));
        assert_eq!(terminal_key(";right"), Some("\x1b[C".to_string()));
        assert_eq!(terminal_key(";left"), Some("\x1b[D".to_string()));
        assert_eq!(terminal_key(";right 5"), Some("\x1b[C".repeat(5)));
        assert_eq!(terminal_key(";up 1"), Some("\x1b[A".to_string()));
        assert_eq!(terminal_key(";esc 3"), Some("\x1b".repeat(3)));
        assert_eq!(terminal_key(";space 4"), Some("    ".to_string()));
        assert_eq!(terminal_key(";ctrl+w"), Some("\x17".to_string()));
        assert_eq!(terminal_key(";ctrl+c"), Some("\x03".to_string()));
        assert_eq!(terminal_key(";ctrl+W"), Some("\x17".to_string()));
        assert_eq!(terminal_key(";ctrl+w 3"), Some("\x17".repeat(3)));
        assert_eq!(terminal_key(".return"), None);
        assert_eq!(terminal_key(".."), None);
        assert_eq!(terminal_key(".backspace"), None);
        assert_eq!(terminal_key("hi"), None);
        assert_eq!(terminal_key(""), None);
        assert_eq!(terminal_key(";right 0"), None);
        assert_eq!(terminal_key(";right 101"), None);
        assert_eq!(terminal_key(";right abc"), None);
        assert_eq!(terminal_key(";right 2 extra"), None);
        assert_eq!(terminal_key("x.right"), None);
        assert_eq!(terminal_key(";ctrl+1"), None);
        assert_eq!(terminal_key(";ctrl+"), None);
        assert_eq!(terminal_key(";ctrl+ab"), None);
    }

    #[test]
    fn sh_escape_quotes_safely() {
        assert_eq!(crate::commands::sh_escape("simple"), "'simple'");
        assert_eq!(crate::commands::sh_escape("a'b"), "'a'\\''b'");
        assert_eq!(crate::commands::sh_escape("/tmp/a b"), "'/tmp/a b'");
    }

    #[test]
    fn typed_input_mixes_keys_and_text() {
        assert_eq!(crate::live::expand_typed_input(";enter hello"), "\r hello");
        assert_eq!(crate::live::expand_typed_input(";up 3"), "\x1b[A".repeat(3));
        assert_eq!(crate::live::expand_typed_input(";up ;down hi"), "\x1b[A\x1b[B hi");
        assert_eq!(crate::live::expand_typed_input(";ctrl+c ls"), "\x03 ls");
        assert_eq!(crate::live::expand_typed_input("echo ;esc done"), "echo\x1b done");
        assert_eq!(crate::live::expand_typed_input("press ;enter to submit"), "press\r to submit");
        assert_eq!(crate::live::expand_typed_input("use ;enter 2 times"), "use\r\r times");
        assert_eq!(crate::live::expand_typed_input("echo a;up"), "echo a;up");
        assert_eq!(crate::live::expand_typed_input("ls\\n;enter pwd"), "ls\n\r pwd");
        assert_eq!(crate::live::expand_typed_input("  spaced   out  "), "  spaced   out  ");
    }

    #[tokio::test]
    async fn cmd_output_times_out_instead_of_hanging() {
        let Some(sleep) = tool_path("sleep") else { return; };
        let s = sleep.to_string_lossy().into_owned();
        let t0 = std::time::Instant::now();
        let r = crate::vm::cmd_output(&s, &["5"], 1).await;
        assert!(r.is_err(), "wedged spawn must time out");
        assert!(t0.elapsed().as_secs() < 5, "returned promptly");
        let ok = crate::vm::cmd_output(&s, &["0"], 5).await;
        assert!(ok.is_ok() && ok.map(|o| o.status.success()).unwrap_or(false));
    }

    #[test]
    fn tool_path_finds_shell_and_rejects_junk() {
        let sh = tool_path("sh");
        assert!(sh.is_some(), "sh must resolve even with a minimal PATH");
        assert!(sh.unwrap().is_absolute());
        assert!(tool_path("definitely-not-a-real-tool-xyz").is_none());
        assert!(tool_path("").is_none());
        assert!(tool_path("a/b").is_none());
    }

    #[test]
    fn render_produces_visible_pixels_for_ansi_output() {
        let Some(reg) = crate::termrender::system_font_bytes("DejaVu Sans Mono") else {
            eprintln!("no system fonts, skipping render smoke test");
            return;
        };
        let bold = crate::termrender::system_font_bytes("DejaVu Sans Mono:weight=bold")
            .unwrap_or_else(|| reg.clone());
        let Some(fonts) = crate::termrender::TermFonts::load(&reg, &bold) else {
            eprintln!("fonts do not parse, skipping render smoke test");
            return;
        };
        let mut out = Vec::new();
        out.extend_from_slice(b"\x1b[0;32m$ jefetch --static\x1b[0m\n");
        out.extend_from_slice("▗▒▓▓ hello world\n".as_bytes());
        out.extend_from_slice(b"plain tail");
        let mut region = None;
        let png = crate::termrender::render_terminal(
            &fonts,
            &out,
            &std::collections::HashMap::new(),
            &mut region,
        )
        .expect("renders");
        let (w, h, rgba) = crate::pngencode::decode_png_rgba(&png).expect("own output decodes");
        assert!(w > 100 && h > 50, "sane dims, got {}x{}", w, h);
        let bg = [0x0bu8, 0x0e, 0x14];
        let nonbg = rgba.chunks_exact(4).filter(|p| p[..3] != bg[..]).count();
        assert!(nonbg > 1000, "text must paint pixels, got {}", nonbg);
    }

    #[test]
    fn new_bytes_since_never_splits_utf8() {
        assert_eq!(crate::termrender::new_bytes_since("a", "ťx"), "ťx");
        assert_eq!(crate::termrender::new_bytes_since("abť", "bťcd"), "cd");
        assert_eq!(crate::termrender::new_bytes_since("x░yz", "░yz12"), "12");
        assert_eq!(crate::termrender::new_bytes_since("old", "old reused tail"), " reused tail");
        assert_eq!(crate::termrender::new_bytes_since("", "fresh"), "fresh");
        assert_eq!(crate::termrender::new_bytes_since("same", "same"), "");
        assert_eq!(crate::termrender::new_bytes_since("abc", "xyz"), "xyz");
    }

    #[test]
    fn tabs_expand_to_spaces() {
        let out = sanitize_ansi("a\tb");
        assert_eq!(out, "a        b", "got {:?}", out);
    }

    #[test]
    fn fit_reports_truncation_flag() {
        let (_, t) = fit_bottom_lines("short\nlines");
        assert!(!t);
        let long = (0..100).map(|i| format!("line {:03} {}", i, "x".repeat(12))).collect::<Vec<_>>().join("\n");
        let (fitted, t) = fit_bottom_lines(&long);
        assert!(t, "long output must flag truncated");
        assert!(fitted.contains("line 099"), "bottom kept");
        assert!(!fitted.contains("line 000"), "head dropped");
    }

    #[test]
    fn file_body_caps_huge_output() {
        let huge = "y".repeat(500_000);
        let capped = cap_file_body(&huge);
        assert!(capped.chars().count() <= 400_100, "got {}", capped.chars().count());
        assert!(capped.starts_with('…'), "marks truncation");
        assert_eq!(cap_file_body("small"), "small");
    }

    #[test]
    fn attach_name_is_safe_filename() {
        assert_eq!(attach_name("jefetch --static"), "jefetch.txt");
        assert_eq!(attach_name(""), "output.txt");
        assert_eq!(attach_name("../../../etc/passwd"), "etcpasswd.txt");
        assert_eq!(attach_name("sudo pacman -Syu"), "sudo.txt");
    }

    #[test]
    fn strip_sgr_drops_everything_escapey() {
        assert_eq!(strip_sgr("\x1b[0;32m$\x1b[0m hi"), "$ hi");
        assert_eq!(strip_sgr("\x1b[0;1m\x1b[0;36m'\x1b[0m"), "'");
        assert_eq!(strip_sgr("\x1b[38;2;1;2;3mX\x1b[0m"), "X");
        assert_eq!(strip_sgr("plain"), "plain");
        assert_eq!(strip_sgr("a\x1b]8;;http://x\x07b"), "ab");
        assert_eq!(strip_sgr("a\x1b"), "a");
        assert_eq!(strip_sgr("a\rb"), "ab");
        let intact = "\x1b[0;32mok\x1b[0m";
        assert!(!strip_sgr(intact).contains('\x1b'), "no ESC remains");
        assert_eq!(strip_sgr(intact), "ok");
    }

    #[test]
    fn parse_channel_accepts_id_only() {
        assert_eq!(parse_channel("123456789"), Some(123456789));
        assert_eq!(parse_channel("  123456789  "), Some(123456789));
        assert_eq!(parse_channel("off"), None);
        assert_eq!(parse_channel(""), None);
        assert_eq!(parse_channel("0"), None);
        assert_eq!(parse_channel("abc"), None);
        assert_eq!(parse_channel("<#123456789>"), None);
    }

    #[test]
    fn parse_target_id_accepts_mention_or_id() {
        assert_eq!(parse_target_id("1546673525392146553"), Some(1546673525392146553));
        assert_eq!(parse_target_id("<@1546673525392146553>"), Some(1546673525392146553));
        assert_eq!(parse_target_id("<@!1546673525392146553>"), Some(1546673525392146553));
        assert_eq!(parse_target_id("  123  "), Some(123));
        assert_eq!(parse_target_id(""), None);
        assert_eq!(parse_target_id("0"), None);
        assert_eq!(parse_target_id("abc"), None);
        assert_eq!(parse_target_id("<@abc>"), None);
    }

    #[test]
    fn parse_message_ref_accepts_id_or_link() {
        assert_eq!(parse_message_ref("123", 99), Some((99, 123)));
        assert_eq!(parse_message_ref("  123  ", 99), Some((99, 123)));
        assert_eq!(
            parse_message_ref("https://discord.com/channels/1/2/3", 99),
            Some((2, 3))
        );
        assert_eq!(
            parse_message_ref("https://discord.com/channels/@me/2/3", 99),
            Some((2, 3))
        );
        assert_eq!(
            parse_message_ref("<https://discord.com/channels/1/2/3>", 99),
            Some((2, 3))
        );
        assert_eq!(parse_message_ref("", 1), None);
        assert_eq!(parse_message_ref("0", 1), None);
        assert_eq!(parse_message_ref("abc", 1), None);
        assert_eq!(parse_message_ref("1/2", 1), None);
        assert_eq!(parse_message_ref("https://discord.com/channels/1/2/0", 99), None);
        assert_eq!(parse_message_ref("https://discord.com/channels/1/2/3/4", 99), None);
        assert_eq!(parse_message_ref("https://discord.com/channels/1/2", 99), None);
    }

    #[test]
    fn boot_art_matches_lol_txt_byte_for_byte() {
        let rows: Vec<&str> = crate::commands::BOOT_ART.lines().collect();
        assert_eq!(rows.len(), 5, "five art rows");
        assert_eq!(rows[0], "          .        :-------:");
        assert_eq!(rows[1], "        ^/ \\^      :Im here:");
        assert_eq!(rows[2], "        ●   ●     <:-------:");
        assert_eq!(rows[3], "       /  ω  \\");
        assert_eq!(rows[4], "      /_/   \\_\\");
    }

    #[test]
    fn scrub_redacts_public_ipv4_only() {
        assert_eq!(scrub_public_ip("ip 203.0.113.7 ok"), "ip [redacted] ok");
        assert_eq!(scrub_public_ip("dns 8.8.8.8"), "dns [redacted]");
        assert_eq!(scrub_public_ip("a 1.2.3.4 b 5.6.7.8"), "a [redacted] b [redacted]");
        assert_eq!(scrub_public_ip("local 192.168.1.5"), "local 192.168.1.5");
        assert_eq!(scrub_public_ip("ten 10.0.0.1"), "ten 10.0.0.1");
        assert_eq!(scrub_public_ip("corp 172.16.5.4 and 172.31.255.1"), "corp 172.16.5.4 and 172.31.255.1");
        assert_eq!(scrub_public_ip("not-private 172.32.0.1"), "not-private [redacted]");
        assert_eq!(scrub_public_ip("loop 127.0.0.1"), "loop 127.0.0.1");
        assert_eq!(scrub_public_ip("link 169.254.169.254"), "link 169.254.169.254");
        assert_eq!(scrub_public_ip("cgnat 100.64.0.1"), "cgnat 100.64.0.1");
        assert_eq!(scrub_public_ip("not-cgnat 100.128.0.1"), "not-cgnat [redacted]");
        assert_eq!(scrub_public_ip("kernel 7.2.2-artix1"), "kernel 7.2.2-artix1");
        assert_eq!(scrub_public_ip("mem 1.48 GiB"), "mem 1.48 GiB");
        assert_eq!(scrub_public_ip("bad 999.1.1.1"), "bad 999.1.1.1");
        assert_eq!(scrub_public_ip("see 1.2.3.4."), "see [redacted].");
        assert_eq!(scrub_public_ip("v1.2.3.4 out"), "v1.2.3.4 out");
        assert_eq!(scrub_public_ip("1.2.3.4.5 out"), "1.2.3.4.5 out");
    }

    #[test]
    fn scrub_redacts_public_ipv6_only() {
        assert_eq!(scrub_public_ip("ip 2001:db8::1 ok"), "ip [redacted] ok");
        assert_eq!(scrub_public_ip("full 2001:db8:0:0:0:0:0:1"), "full [redacted]");
        assert_eq!(scrub_public_ip("ll fe80::1"), "ll fe80::1");
        assert_eq!(scrub_public_ip("ll FE80::A"), "ll FE80::A");
        assert_eq!(scrub_public_ip("ula fd00::5"), "ula fd00::5");
        assert_eq!(scrub_public_ip("ula fc12::9"), "ula fc12::9");
        assert_eq!(scrub_public_ip("lo ::1"), "lo ::1");
        assert_eq!(scrub_public_ip("lo 0:0:0:0:0:0:0:1"), "lo 0:0:0:0:0:0:0:1");
        assert_eq!(scrub_public_ip("x :: y"), "x :: y");
        assert_eq!(scrub_public_ip("mac aa:bb:cc:dd:ee:ff"), "mac aa:bb:cc:dd:ee:ff");
        assert_eq!(scrub_public_ip("at 12:34:56"), "at 12:34:56");
        assert_eq!(scrub_public_ip("mapped ::ffff:203.0.113.7"), "mapped [redacted]");
        assert_eq!(scrub_public_ip("mc ff02::1"), "mc [redacted]");
        assert_eq!(scrub_public_ip("bracketed [2001:db8::1]"), "bracketed [[redacted]]");
        assert_eq!(scrub_public_ip("port [2001:db8::1]:443"), "port [[redacted]]:443");
        assert_eq!(scrub_public_ip("v4 still 8.8.8.8 ok"), "v4 still [redacted] ok");
        assert_eq!(scrub_public_ip("v4 local 192.168.0.1 ok"), "v4 local 192.168.0.1 ok");
    }

    #[test]
    fn fg_survives() {
        let out = sanitize_ansi("\x1b[0;32mok\x1b[0m");
        assert_eq!(out, "\x1b[0;32mok\x1b[0m");
    }

    #[test]
    fn runas_validation_blocks_root_and_junk() {
        assert!(valid_runas("matko"));
        assert!(valid_runas("u123"));
        assert!(valid_runas("a-b_c"));
        assert!(!valid_runas("root"));
        assert!(!valid_runas(""));
        assert!(!valid_runas("0abc"));
        assert!(!valid_runas("a/b"));
        assert!(!valid_runas("a b"));
        assert!(!valid_runas("ABC"));
        assert!(!valid_runas(&"a".repeat(33)));
    }

    #[test]
    fn sudoers_script_grants_nopasswd_safely() {
        let s = crate::commands::sudoers_script("matko802").expect("valid name");
        assert!(s.contains("u='matko802'"), "pins user, got {:?}", s);
        assert!(s.contains("ALL=(ALL) NOPASSWD: ALL"), "got {:?}", s);
        assert!(s.contains("!requiretty"), "no-tty sudo, got {:?}", s);
        assert!(s.contains("/etc/sudoers.d/"), "drop-in, got {:?}", s);
        assert!(s.contains("chmod 0440"), "tight perms so sudo accepts it, got {:?}", s);
        assert!(!s.contains("chmod 666") && !s.contains("chmod 777"), "never world-writable, got {:?}", s);
        assert!(s.contains("visudo -c"), "validates, got {:?}", s);
        assert!(s.contains("wheel"), "wheel/sudo group, got {:?}", s);
        assert!(crate::commands::sudoers_script("root").is_none(), "never sudo root");
        assert!(crate::commands::sudoers_script("a/b").is_none());
        assert!(crate::commands::sudoers_script("a b").is_none());
        assert!(crate::commands::sudoers_script("").is_none());
    }

    #[test]
    fn live_edits_are_throttled_and_tolerant() {
        assert_eq!(
            crate::live::LIVE_EDIT_MIN_INTERVAL.as_millis(),
            2000,
            "updates at most every 2s"
        );
        assert!(
            crate::live::LIVE_EDIT_MAX_FAILS > 1,
            "transient 429s tolerated, got {}",
            crate::live::LIVE_EDIT_MAX_FAILS
        );
    }

    #[test]
    fn live_closed_notice_is_code_markdown() {
        assert_eq!(
            crate::live::live_closed_text(),
            "```\nThis live session has been closed.\n```"
        );
    }

    #[test]
    fn live_end_closes_only_clean_image_sessions() {
        use crate::live::live_end_closes;
        assert!(live_end_closes(0, true, true));
        assert!(!live_end_closes(1, true, true), "failures keep output");
        assert!(!live_end_closes(-1, true, true), "failures keep output");
        assert!(!live_end_closes(0, false, true), "quick text results kept");
        assert!(!live_end_closes(0, true, false), "text mode has no image to clear");
    }

    #[test]
    fn frame_due_posts_changes_on_steady_cadence() {
        use crate::live::frame_due;
        assert!(!frame_due(None, 9, Some(100), 2000));
        assert!(!frame_due(Some(7), 7, Some(5000), 2000));
        assert!(!frame_due(Some(5), 7, Some(500), 2000));
        assert!(frame_due(Some(5), 7, Some(2000), 2000));
        assert!(frame_due(None, 9, None, 2000));
    }

    #[test]
    fn slow_cycle_note_flags_only_slow_loops() {
        assert!(crate::live::slow_cycle_note(100, 200, 300).is_none());
        let note = crate::live::slow_cycle_note(500, 800, 2000).expect("slow");
        assert!(note.contains("3300ms"), "got {:?}", note);
        assert!(note.contains("fetch 500ms"), "got {:?}", note);
    }

    #[test]
    fn random_suffix_looks_unique_hex() {
        let a = random_suffix();
        let b = random_suffix();
        assert!(!a.is_empty());
        assert!(a.len() >= 8, "got {:?}", a);
        assert_ne!(a, b, "suffix should differ per call");
    }
}

#[tokio::main]
async fn main() {
    let fresh_config = !config_file_path().exists();
    ensure_config_template();
    let mut file_config = load_file_config();
    let token: String = match file_config.discord_token.clone() {
        Some(t) => t,
        None => std::env::var("DISCORD_TOKEN").unwrap_or_else(|_| {
            eprintln!(
                "error: set discord_token in {} or DISCORD_TOKEN env",
                config_file_path().display()
            );
            std::process::exit(1);
        }),
    };
    let owner: u64 = match file_config.owner_id {
        Some(id) => id,
        None => std::env::var("OWNER_ID")
            .unwrap_or_else(|_| {
                eprintln!(
                    "error: set owner_id in {} or OWNER_ID env",
                    config_file_path().display()
                );
                std::process::exit(1);
            })
            .parse()
            .unwrap_or_else(|_| {
                eprintln!("error: OWNER_ID must be a number");
                std::process::exit(1);
            }),
    };
    if fresh_config {
        let legacy_users: AllowedFile = tokio::fs::read_to_string("users.json")
            .await
            .ok()
            .and_then(|r| serde_json::from_str(&r).ok())
            .unwrap_or_default();
        let legacy_shells: std::collections::HashMap<String, String> =
            tokio::fs::read_to_string("shells.json")
                .await
                .ok()
                .and_then(|r| serde_json::from_str(&r).ok())
                .unwrap_or_default();
        let legacy_notify: Option<u64> = tokio::fs::read_to_string("settings.json")
            .await
            .ok()
            .and_then(|r| serde_json::from_str::<BotSettings>(&r).ok())
            .and_then(|s| s.notify_channel);
        apply_legacy_import(&mut file_config, legacy_users, legacy_shells, legacy_notify);
        if let Ok(text) = toml::to_string_pretty(&file_config) {
            let p = config_file_path();
            let ps = p.to_string_lossy().to_string();
            let _ = tokio::fs::remove_file(&p).await;
            let _ = save_json(&ps, text).await;
        }
    }
    crate::webhook::init_webhook_urls(file_config.webhook_urls);
    let _ = std::env::set_current_dir(project_dir());
    let vm: String = file_config
        .vm_name
        .clone()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            std::env::var("VM_NAME")
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
        .unwrap_or_else(|| {
            eprintln!(
                "warning: vm_name not set in {} or VM_NAME env — VM commands will reply with a friendly error until you set it",
                config_file_path().display()
            );
            String::new()
        });
    let data = Data {
        allowed: std::sync::Arc::new(tokio::sync::RwLock::new(Allowed {
            owner,
            users: file_config.managers.clone(),
            linux: file_config.linux.clone(),
            blocked: file_config.blocked_ids.clone(),
            admins: file_config.admin_ids.clone(),
        })),
        vm: std::sync::Arc::new(tokio::sync::RwLock::new(vm)),
        live: Default::default(),
        settings: std::sync::Arc::new(tokio::sync::RwLock::new(BotSettings {
            notify_channel: file_config.notify_channel,
            war_mode: file_config.war_mode,
            sayas_enabled: file_config.sayas_enabled,
            ai_enabled: file_config.ai_enabled,
            ai_model: if crate::ai::valid_model_name(&file_config.ai_model) {
                file_config.ai_model.clone()
            } else {
                crate::ai::default_model()
            },
            ollama_host: if file_config.ollama_host.trim().is_empty() {
                crate::ai::default_host()
            } else {
                file_config.ollama_host.clone()
            },
            ollama_api_key: file_config.ollama_api_key.trim().to_string(),
        })),
        shells: std::sync::Arc::new(tokio::sync::RwLock::new(file_config.shells.clone())),
    };
    tokio::spawn(crate::config::watch_config(data.clone()));

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: vec![
                help(),
                ps(),
                status(),
                start(),
                stop(),
                restart(),
                info(),
                user(),
                admin(),
                shell(),
                botrestart(),
                run(),
                sayas(),
                send(),
                notify(),
                purge_replies(),
                warmode(),
                upload(),
                ai(),
                websearch(),
            ],
            on_error: |error| {
                Box::pin(async move {
                    eprintln!("framework error: {}", error);
                    if let poise::FrameworkError::Command { ctx, .. } = error {
                        let _ = ctx
                            .say("Something broke on my side — check the terminal log.")
                            .await;
                    }
                })
            },
            event_handler: |ctx, event, framework, data| {
                Box::pin(events::event_handler(ctx, event, framework, data))
            },
            ..Default::default()
        })
        .setup(|ctx, _ready, framework| {
            Box::pin(async move {
                poise::builtins::register_globally(ctx, &framework.options().commands).await?;
                let stale_vm = data.vm.read().await.clone();
                crate::live::cleanup_stale_live_files(&stale_vm).await;
                if let Some(ch) = data.settings.read().await.notify_channel {
                    let _ = crate::webhook::post_message(
                        &ctx.http,
                        serenity::ChannelId::new(ch),
                        format!("```\n{BOOT_ART}\n```"),
                        Vec::new(),
                    )
                    .await;
                }
                Ok(data)
            })
        })
        .build();

    let intents =
        serenity::GatewayIntents::non_privileged() | serenity::GatewayIntents::MESSAGE_CONTENT;
    let mut client = serenity::ClientBuilder::new(token, intents)
        .framework(framework)
        .await
        .expect("client build");
    client.start().await.expect("client start");
}

