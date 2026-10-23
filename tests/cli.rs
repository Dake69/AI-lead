#![cfg(unix)]
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Output},
};

fn cli(root: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ai-lead"));
    cmd.arg("--project").arg(root);
    cmd
}

fn setup(provider: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let out = cli(dir.path())
        .args(["init", "--provider", provider])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Existing provider fixtures exercise restricted/read-only runs explicitly.
    let config = dir.path().join(".ai-lead/team.toml");
    fs::write(
        &config,
        fs::read_to_string(&config).unwrap().replace(
            r#"permissions = "full-access""#,
            r#"permissions = "restricted""#,
        ),
    )
    .unwrap();
    let bin = dir.path().join("bin");
    fs::create_dir(&bin).unwrap();
    for name in ["codex", "claude"] {
        let path = bin.join(name);
        fs::write(&path, include_str!("fixtures/provider.sh")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    dir
}

fn run(root: &Path, mode: &str) -> Output {
    let path = std::env::join_paths(std::iter::once(root.join("bin")).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .unwrap();
    cli(root)
        .env("PATH", path)
        .env("MOCK_MODE", mode)
        .args(["run", "--no-tui", "Build integration"])
        .output()
        .unwrap()
}

fn session(root: &Path) -> std::path::PathBuf {
    fs::read_dir(root.join(".ai-lead/runs"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
}

#[test]
fn both_providers_route_peer_messages_and_return_control_to_lead() {
    for provider in ["codex", "claude"] {
        let dir = setup(provider);
        let out = run(dir.path(), "success");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let s = session(dir.path());
        let events = fs::read_to_string(s.join("events.jsonl")).unwrap();
        assert!(events.contains("CONTRACT_V1"));
        assert!(events.contains("Leader verified all reports"));
        let prompt = fs::read_to_string(s.join("001-001-frontend/prompt.txt")).unwrap();
        assert!(prompt.contains("CONTRACT_V1"));
        assert!(prompt.contains("# Frontend"));
        let leader = fs::read_to_string(s.join("002-lead/prompt.txt")).unwrap();
        assert!(leader.contains("Backend contract ready"));
        assert!(leader.contains("Frontend received CONTRACT_V1"));
        assert!(
            fs::read_to_string(s.join("status.json"))
                .unwrap()
                .contains("COMPLETE")
        );
        assert!(
            fs::read_to_string(s.join("transcript.json"))
                .unwrap()
                .contains("Leader verified all reports")
        );
    }
}

#[test]
fn specialist_failure_is_returned_to_the_lead() {
    let dir = setup("codex");
    let out = run(dir.path(), "worker-error");
    assert!(
        !out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = session(dir.path());
    let prompt = fs::read_to_string(s.join("002-lead/prompt.txt")).unwrap();
    assert!(prompt.contains("backend failed"));
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("Leader acknowledged specialist failure")
    );
}

#[test]
fn claude_error_result_is_not_mistaken_for_success() {
    let dir = setup("claude");
    let out = run(dir.path(), "auth-error");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("Provider reported failure"));
    assert!(
        fs::read_to_string(session(dir.path()).join("status.json"))
            .unwrap()
            .contains("FAILED")
    );
}

#[test]
fn round_limit_leaves_task_incomplete() {
    let dir = setup("codex");
    let path = dir.path().join(".ai-lead/team.toml");
    let c = fs::read_to_string(&path)
        .unwrap()
        .replace("max_rounds = 12", "max_rounds = 1");
    fs::write(path, c).unwrap();
    let out = run(dir.path(), "success");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("Round limit reached"));
}

#[test]
fn provider_timeout_terminates_run() {
    let dir = setup("codex");
    let path = dir.path().join(".ai-lead/team.toml");
    let c = fs::read_to_string(&path)
        .unwrap()
        .replace("timeout_seconds = 900", "timeout_seconds = 1");
    fs::write(path, c).unwrap();
    let start = std::time::Instant::now();
    let out = run(dir.path(), "timeout");
    assert!(!out.status.success());
    assert!(start.elapsed().as_secs() < 5);
    assert!(String::from_utf8_lossy(&out.stderr).contains("timed out"));
}

#[test]
fn cancellation_stops_provider_and_saves_status() {
    let dir = setup("codex");
    let path = std::env::join_paths(std::iter::once(dir.path().join("bin")).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .unwrap();
    let mut child = cli(dir.path())
        .env("PATH", path)
        .env("MOCK_MODE", "timeout")
        .args(["run", "--no-tui", "Wait until cancellation"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let marker = dir.path().join(".ai-lead/mock-provider.pid");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !marker.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    if !marker.exists() {
        let _ = child.kill();
        panic!("Mock provider did not start");
    }
    let pid: i32 = fs::read_to_string(marker).unwrap().trim().parse().unwrap();
    unsafe {
        libc::kill(child.id() as i32, libc::SIGINT);
    }
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            panic!("Cancellation did not stop the run");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert!(status.success());
    assert!(
        fs::read_to_string(session(dir.path()).join("status.json"))
            .unwrap()
            .contains("CANCELLED")
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while unsafe { libc::kill(pid, 0) } == 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_ne!(
        unsafe { libc::kill(pid, 0) },
        0,
        "Provider remained alive after cancellation"
    );
}

#[test]
fn restart_restores_team_memory_and_does_not_repeat_completed_tasks() {
    for provider in ["codex", "claude"] {
        let dir = setup(provider);
        let provider_path = std::env::join_paths(std::iter::once(dir.path().join("bin")).chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        ))
        .unwrap();
        let mut child = cli(dir.path())
            .env("PATH", &provider_path)
            .env("MOCK_MODE", "pause-frontend")
            .args(["run", "--no-tui", "Build integration"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let marker = dir.path().join(".ai-lead/mock-frontend.pid");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        while !marker.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if !marker.exists() {
            let _ = child.kill();
            panic!("Frontend did not reach interrupted checkpoint");
        }
        let pid: i32 = fs::read_to_string(&marker).unwrap().trim().parse().unwrap();
        // Abrupt wrapper death plus removal of its tool group simulates all processes
        // terminating on reboot, without letting the normal cancellation handler run.
        child.kill().unwrap();
        child.wait().unwrap();
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
        let state_path = dir.path().join(".ai-lead/memory/state.json");
        let before: serde_json::Value =
            serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
        let workflow = before["active"].as_str().unwrap();
        assert_eq!(before["workflows"][workflow]["cursor"], 1);
        assert_eq!(
            before["workflows"][workflow]["tasks"][1]["status"],
            "complete"
        );
        let out = cli(dir.path())
            .env("PATH", &provider_path)
            .env("MOCK_MODE", "success")
            .args(["resume", "--no-tui"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let calls = fs::read_to_string(dir.path().join(".ai-lead/mock-calls.log")).unwrap();
        assert_eq!(
            calls.lines().filter(|s| *s == "backend").count(),
            1,
            "Completed backend work was repeated"
        );
        assert_eq!(calls.lines().filter(|s| *s == "frontend").count(), 2);
        let after: serde_json::Value =
            serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
        assert_eq!(after["workflows"][workflow]["status"], "complete");
        assert_eq!(
            after["agents"]["backend"]["facts"][0]["text"],
            "Backend remembers CONTRACT_V1"
        );
        let resumed_dir = after["workflows"][workflow]["session"].as_str().unwrap();
        let prompt =
            fs::read_to_string(Path::new(resumed_dir).join("001-001-frontend/prompt.txt")).unwrap();
        assert!(prompt.contains("Backend remembers CONTRACT_V1"));
        assert!(prompt.contains("Backend published CONTRACT_V1"));
        assert!(prompt.contains("Verify frontend integration"));
        let memory = cli(dir.path())
            .args(["memory", "backend"])
            .output()
            .unwrap();
        assert!(memory.status.success());
        assert!(String::from_utf8_lossy(&memory.stdout).contains("Backend remembers CONTRACT_V1"));
    }
}

#[test]
fn api_init_requires_model_and_doctor_checks_env_without_network() {
    let dir = tempfile::tempdir().unwrap();
    let out = cli(dir.path())
        .args(["init", "--provider", "openai"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(!dir.path().join(".ai-lead/team.toml").exists());
    let out = cli(dir.path())
        .args([
            "init",
            "--provider",
            "openai",
            "--model",
            "my-model",
            "--api-key-env",
            "AI_LEAD_TEST_KEY",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = cli(dir.path())
        .arg("doctor")
        .env_remove("AI_LEAD_TEST_KEY")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("AI_LEAD_TEST_KEY"));
    let out = cli(dir.path())
        .arg("doctor")
        .env("AI_LEAD_TEST_KEY", "mock-secret-do-not-save")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        !fs::read_to_string(dir.path().join(".ai-lead/team.toml"))
            .unwrap()
            .contains("mock-secret")
    );
}

#[test]
fn mixed_api_lead_and_cli_workers_share_messages_and_durable_memory() {
    use serde_json::{Value, json};
    use std::{
        io::{Read, Write},
        net::TcpListener,
    };
    let dir = setup("codex");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let path = dir.path().join(".ai-lead/team.toml");
    let mut config: toml::Value = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    config["leader"]["provider"] = toml::Value::String("openai".into());
    config["leader"].as_table_mut().unwrap().insert(
        "model".into(),
        toml::Value::String("mock-lead-model".into()),
    );
    config["leader"].as_table_mut().unwrap().insert(
        "api".into(),
        toml::Value::try_from(json!({"base_url":url,"api_key_env":"AI_LEAD_MOCK_API_KEY"}))
            .unwrap(),
    );
    fs::write(&path, toml::to_string_pretty(&config).unwrap()).unwrap();
    let server = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for step in 0..2 {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((s, _)) => break s,
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            && std::time::Instant::now() < deadline =>
                    {
                        std::thread::sleep(std::time::Duration::from_millis(10))
                    }
                    Err(e) => panic!("mock accept: {e}"),
                }
            };
            // macOS may inherit O_NONBLOCK from the listening socket.
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            let end = loop {
                let mut b = [0; 4096];
                let n = stream.read(&mut b).unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&b[..n]);
                if let Some(i) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    break i + 4;
                }
            };
            let headers = String::from_utf8_lossy(&bytes[..end]);
            assert!(
                headers
                    .to_lowercase()
                    .contains("authorization: bearer mock-secret-do-not-save")
            );
            let len: usize = headers
                .lines()
                .find_map(|s| {
                    s.to_lowercase()
                        .strip_prefix("content-length:")
                        .map(|s| s.trim().parse().unwrap())
                })
                .unwrap();
            while bytes.len() < end + len {
                let mut b = [0; 4096];
                let n = stream.read(&mut b).unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&b[..n]);
            }
            let request: Value = serde_json::from_slice(&bytes[end..end + len]).unwrap();
            requests.push(request);
            let memory = json!({"facts":["API lead remembers team work"],"next_steps":[],"blockers":[],"shared":["API lead shared plan"]});
            let result = if step == 0 {
                json!({"action":"delegate","assignments":[{"agent":"backend","task":"Implement API contract"},{"agent":"frontend","task":"Consume backend contract"}],"messages":[],"summary":"API lead delegated","memory":memory})
            } else {
                json!({"action":"finish","assignments":[],"messages":[],"summary":"Mixed team completed","memory":memory})
            };
            let body=json!({"status":"completed","output":[{"type":"function_call","call_id":"result","name":"submit_result","arguments":result.to_string()}]}).to_string();
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
        }
        requests
    });
    let binpath = std::env::join_paths(std::iter::once(dir.path().join("bin")).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .unwrap();
    let out = cli(dir.path())
        .env("PATH", binpath)
        .env("AI_LEAD_MOCK_API_KEY", "mock-secret-do-not-save")
        .args(["run", "--no-tui", "Build mixed team"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let requests = server.join().unwrap();
    assert!(
        requests[1]
            .to_string()
            .contains("Backend implemented contract")
    );
    assert!(
        requests[1]
            .to_string()
            .contains("Frontend received CONTRACT_V1")
    );
    let state = fs::read_to_string(dir.path().join(".ai-lead/memory/state.json")).unwrap();
    assert!(state.contains("API lead remembers team work"));
    assert!(state.contains("Backend remembers CONTRACT_V1"));
    for artifact in [
        path,
        session(dir.path()).join("session.json"),
        session(dir.path()).join("events.jsonl"),
        dir.path().join(".ai-lead/memory/state.json"),
    ] {
        assert!(
            !fs::read_to_string(artifact)
                .unwrap()
                .contains("mock-secret-do-not-save")
        );
    }
}

#[test]
fn full_access_is_blocked_until_consent_and_can_be_revoked() {
    let dir = setup("codex");
    let config = dir.path().join(".ai-lead/team.toml");
    fs::write(
        &config,
        fs::read_to_string(&config).unwrap().replace(
            r#"permissions = "restricted""#,
            r#"permissions = "full-access""#,
        ),
    )
    .unwrap();
    let binpath = std::env::join_paths(std::iter::once(dir.path().join("bin")).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .unwrap();
    let run = || {
        cli(dir.path())
            .env("PATH", &binpath)
            .args(["run", "--no-tui", "Build integration"])
            .output()
            .unwrap()
    };
    let before = run();
    assert!(!before.status.success());
    assert!(String::from_utf8_lossy(&before.stderr).contains("requires project consent"));
    assert!(!dir.path().join(".ai-lead/mock-calls.log").exists());
    let grant = cli(dir.path())
        .args(["permissions", "full-access", "--yes"])
        .output()
        .unwrap();
    assert!(
        grant.status.success(),
        "{}",
        String::from_utf8_lossy(&grant.stderr)
    );
    let out = run();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let metadata = fs::read_to_string(session(dir.path()).join("session.json")).unwrap();
    assert!(metadata.contains("\"full_access\": true"));
    let out = cli(dir.path())
        .args(["permissions", "restricted"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(!dir.path().join(".ai-lead/permissions.json").exists());
    assert!(
        fs::read_to_string(dir.path().join(".ai-lead/team.toml"))
            .unwrap()
            .contains("permissions = \"restricted\"")
    );
}
