use crate::{config::Data, util::valid_runas, Error};

pub(crate) async fn virsh(args: &[&str]) -> Result<String, Error> {
    let out = tokio::process::Command::new("virsh")
        .args(["--connect", "qemu:///system"])
        .args(args)
        .output()
        .await?;
    if !out.status.success() {
        return Err(format!(
            "virsh {} failed:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub(crate) async fn agent_ping(vm: &str) -> bool {
    let out = tokio::process::Command::new("virsh")
        .args([
            "--connect",
            "qemu:///system",
            "qemu-agent-command",
            vm,
            "{\"execute\":\"guest-ping\"}",
        ])
        .output()
        .await;
    matches!(out, Ok(o) if o.status.success())
}

pub(crate) async fn wait_agent(vm: &str, secs: u64) -> bool {
    for _ in 0..secs.max(1) {
        if agent_ping(vm).await {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    agent_ping(vm).await
}

pub(crate) async fn guest_exec(
    vm: &str,
    path: &str,
    args: &[&str],
    capture: bool,
    timeout_s: u64,
) -> Result<(i64, String, String), Error> {
    use base64::Engine as _;
    let pid = guest_launch_raw(vm, path, args, capture).await?;
    for _ in 0..timeout_s.max(1) {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let st = tokio::process::Command::new("virsh")
            .args([
                "--connect",
                "qemu:///system",
                "qemu-agent-command",
                vm,
                &serde_json::json!({"execute":"guest-exec-status","arguments":{"pid":pid}})
                    .to_string(),
            ])
            .output()
            .await?;
        if !st.status.success() {
            continue;
        }
        if st.stdout.iter().all(|b| b.is_ascii_whitespace()) {
            continue;
        }
        let s: serde_json::Value = serde_json::from_slice(&st.stdout)
            .map_err(|e| format!("status: {}", e))?;
        if !s["return"]["exited"].as_bool().unwrap_or(false) {
            continue;
        }
        let code = s["return"]["exitcode"].as_i64().unwrap_or(-1);
        if !capture {
            return Ok((code, String::new(), String::new()));
        }
        let dec = |v: &serde_json::Value| {
            v.as_str()
                .and_then(|b| {
                    base64::engine::general_purpose::STANDARD.decode(b).ok()
                })
                .map(|b| String::from_utf8_lossy(&b).to_string())
                .unwrap_or_default()
        };
        return Ok((
            code,
            dec(&s["return"]["out-data"]),
            dec(&s["return"]["err-data"]),
        ));
    }
    Err("guest-exec timed out waiting for exit (it may still be running in the guest)".into())
}

pub(crate) async fn guest_launch_raw(vm: &str, path: &str, args: &[&str], capture: bool) -> Result<i64, Error> {
    for _ in 0..3 {
        let out = tokio::process::Command::new("virsh")
            .args([
                "--connect",
                "qemu:///system",
                "qemu-agent-command",
                vm,
                &serde_json::json!({
                    "execute": "guest-exec",
                    "arguments": { "path": path, "arg": args, "capture-output": capture }
                })
                .to_string(),
            ])
            .output()
            .await?;
        if !out.status.success() {
            return Err(format!(
                "guest-exec launch failed:\n{}",
                String::from_utf8_lossy(&out.stderr).trim()
            )
            .into());
        }
        if out.stdout.iter().all(|b| b.is_ascii_whitespace()) {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            continue;
        }
        let v: serde_json::Value = serde_json::from_slice(&out.stdout)
            .map_err(|e| format!("launch: {}", e))?;
        return v["return"]["pid"]
            .as_i64()
            .ok_or_else(|| "guest-exec: no pid returned".into());
    }
    Err("launch: agent returned empty response 3x".into())
}

pub(crate) async fn user_shell(data: &Data, uid: u64) -> String {
    data.shells
        .read()
        .await
        .get(&uid.to_string())
        .cloned()
        .unwrap_or_else(|| "bash".into())
}

pub(crate) async fn linked_user(data: &Data, uid: u64) -> Option<String> {
    data.allowed
        .read()
        .await
        .linux
        .get(&uid.to_string())
        .cloned()
}

pub(crate) async fn run_guest_cmd(vm: &str, shell: &str, cmd_text: &str, runas: Option<&str>, timeout_s: u64) -> Result<(String, i64), Error> {
    if let Some(u) = runas {
        if !valid_runas(u) {
            return Ok((
                "Linked linux account is invalid; ask the owner to re-add you.".into(),
                -1,
            ));
        }
    }
    if !agent_ping(vm).await {
        return Ok((
            "Guest agent is silent. Install `qemu-guest-agent` in Artix first.".into(),
            -1,
        ));
    }
    let (sh_path, setup, guard) = if shell == "fish" {
        ("/usr/sbin/fish", "set -gx SHELL /usr/sbin/fish; set -gx PATH $HOME/.local/bin $HOME/bin /usr/local/bin $PATH; ", "$status")
    } else {
        ("/bin/bash", "export SHELL=/bin/bash PATH=\"$HOME/.local/bin:$HOME/bin:/usr/local/bin:$PATH\"; ", "$?")
    };
    let inner = cmd_text.trim().trim_end_matches(';').trim_end();
    let shcmd = format!("{}{}; exit {}", setup, inner, guard);
    let (lpath, largs): (&str, Vec<&str>) = match runas {
        Some(u) => ("su", vec![u, "-s", sh_path, "-c", &shcmd]),
        None => (sh_path, vec!["-c", &shcmd]),
    };
    let run = guest_exec(vm, lpath, &largs, true, timeout_s).await;
    let run = match run {
        Err(e) if e.to_string().contains("No such file") && sh_path != "/bin/bash" => {
            let fallback =
                format!("export SHELL=/bin/bash; {}; exit $?", inner);
            let (lpath2, largs2): (&str, Vec<&str>) = match runas {
                Some(u) => ("su", vec![u, "-s", "/bin/bash", "-c", &fallback]),
                None => ("/bin/bash", vec!["-c", &fallback]),
            };
            guest_exec(vm, lpath2, &largs2, true, timeout_s).await
        }
        other => other,
    };
    match run {
        Ok((code, out, err)) => {
            let mut body = format!(
                "\u{1b}[0;32m$ {}\u{1b}[0m\n{}",
                cmd_text.trim(),
                out.trim_end()
            );
            if !err.trim().is_empty() {
                body.push_str(&format!(
                    "\n\u{1b}[0;31mstderr:\u{1b}[0m\n{}",
                    err.trim_end()
                ));
            }
            if code != 0 {
                body.push_str(&format!("\n\u{1b}[0;31mexit {}\u{1b}[0m", code));
            }
            Ok((body, code))
        }
        Err(e) => Ok((e.to_string(), -1)),
    }
}

