use crate::{config::Data, Error};

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

pub(crate) async fn guest_status(vm: &str, pid: i64) -> Result<Option<i64>, Error> {
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
        return Ok(None);
    }
    if st.stdout.iter().all(|b| b.is_ascii_whitespace()) {
        return Ok(None);
    }
    let s: serde_json::Value = serde_json::from_slice(&st.stdout)
        .map_err(|e| format!("status poll: {}", e))?;
    if s["return"]["exited"].as_bool().unwrap_or(false) {
        Ok(Some(s["return"]["exitcode"].as_i64().unwrap_or(-1)))
    } else {
        Ok(None)
    }
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
    // Check-first loop with a deadline: instant commands (tail, rm, cat)
    // return without the old mandatory 1s sleep, slow ones poll every 1s.
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(timeout_s.max(1));
    loop {
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
        if st.status.success() && !st.stdout.iter().all(|b| b.is_ascii_whitespace()) {
            if let Ok(s) = serde_json::from_slice::<serde_json::Value>(&st.stdout) {
                if s["return"]["exited"].as_bool().unwrap_or(false) {
                    let code = s["return"]["exitcode"].as_i64().unwrap_or(-1);
                    if !capture {
                        return Ok((code, String::new(), String::new()));
                    }
                    let dec = |v: &serde_json::Value| {
                        v.as_str()
                            .and_then(|b| {
                                base64::engine::general_purpose::STANDARD
                                    .decode(b)
                                    .ok()
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
            }
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
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

pub(crate) async fn linked_user(data: &Data, uid: u64) -> Option<String> {
    data.allowed
        .read()
        .await
        .linux
        .get(&uid.to_string())
        .cloned()
}

