#![cfg(unix)]

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

fn cli(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ai-lead"));
    command.arg("--project").arg(root);
    command
}

fn setup() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let output = cli(dir.path()).arg("init").output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let path = dir.path().join(".ai-lead/team.toml");
    let mut config: toml::Value = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    config["permissions"] = toml::Value::String("restricted".into());
    config
        .as_table_mut()
        .unwrap()
        .insert("max_parallel_agents".into(), toml::Value::Integer(4));
    fs::write(path, toml::to_string_pretty(&config).unwrap()).unwrap();
    let bin = dir.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let codex = bin.join("codex");
    fs::write(&codex, include_str!("fixtures/parallel-provider.sh")).unwrap();
    fs::set_permissions(codex, fs::Permissions::from_mode(0o755)).unwrap();
    dir
}

struct Running {
    child: Child,
    root: PathBuf,
    terminated: bool,
}

impl Running {
    fn spawn(root: &Path, mode: &str, resume: bool) -> Self {
        let path = std::env::join_paths(std::iter::once(root.join("bin")).chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        ))
        .unwrap();
        let mut command = cli(root);
        command.env("PATH", path).env("PARALLEL_MODE", mode);
        if resume {
            command.args(["resume", "--no-tui"]);
        } else {
            command.args(["run", "--no-tui", "Build deterministic parallel project"]);
        }
        command.stdout(Stdio::from(
            fs::File::create(root.join("stdout.log")).unwrap(),
        ));
        command.stderr(Stdio::from(
            fs::File::create(root.join("stderr.log")).unwrap(),
        ));
        Self {
            child: command.spawn().unwrap(),
            root: root.into(),
            terminated: false,
        }
    }

    fn marker(&self, name: &str) -> PathBuf {
        self.root.join(".ai-lead/parallel").join(name)
    }

    fn wait_marker(&mut self, name: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.marker(name).exists() {
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "Run exited waiting for {name}: {}",
                self.logs()
            );
            assert!(
                Instant::now() < deadline,
                "Barrier {name} not reached: {}",
                self.logs()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn release(&self, role: &str) {
        fs::write(self.marker(&format!("release-{role}")), "").unwrap();
    }

    fn logs(&self) -> String {
        format!(
            "stdout: {}\nstderr: {}",
            fs::read_to_string(self.root.join("stdout.log")).unwrap_or_default(),
            fs::read_to_string(self.root.join("stderr.log")).unwrap_or_default()
        )
    }

    fn success(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "{}", self.logs());
                self.terminated = true;
                break;
            }
            assert!(
                Instant::now() < deadline,
                "Run did not finish: {}",
                self.logs()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn terminate(&mut self) {
        if self.terminated {
            return;
        }
        self.terminated = true;
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Ok(entries) = fs::read_dir(self.root.join(".ai-lead/parallel")) {
            for entry in entries.flatten() {
                if entry.path().extension().is_some_and(|ext| ext == "pid")
                    && let Ok(pid) = fs::read_to_string(entry.path())
                        .unwrap_or_default()
                        .trim()
                        .parse::<i32>()
                {
                    unsafe {
                        libc::kill(-pid, libc::SIGKILL);
                    }
                }
            }
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.terminate();
    }
}

fn state(root: &Path) -> serde_json::Value {
    serde_json::from_slice(&fs::read(root.join(".ai-lead/memory/state.json")).unwrap()).unwrap()
}

#[test]
fn input_during_lead_inference_discards_stale_completion_and_reaches_a_fresh_plan() {
    let dir = setup();
    let mut run = Running::spawn(dir.path(), "steering", false);
    run.wait_marker("lead.planning");
    let state = state(dir.path());
    let session = state["workflows"][state["active"].as_str().unwrap()]["session"]
        .as_str()
        .unwrap();
    fs::write(
        Path::new(session).join("team/user.jsonl"),
        "{\"to\":\"lead\",\"body\":\"STEER_INPUT: add server validation\"}\n",
    )
    .unwrap();
    run.release("lead");
    run.wait_marker("backend.started");
    run.release("backend");
    run.success();
    let restored = self::state(dir.path());
    let workflow = &restored["workflows"][restored["active"].as_str().unwrap()];
    assert_eq!(workflow["status"], "complete");
    assert!(
        workflow["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|task| task["status"] == "superseded")
    );
    assert_eq!(
        workflow["transcript"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["from"] == "user")
            .count(),
        1
    );
    assert!(!run.logs().contains("Lead stopped"));
}

#[test]
fn tester_and_reviewer_start_during_writing_even_when_listed_last_with_a_full_pool() {
    let dir = setup();
    let mut run = Running::spawn(dir.path(), "live-review", false);
    for role in ["reviewer", "tester", "backend", "frontend"] {
        run.wait_marker(&format!("{role}.started"));
    }
    run.wait_marker("tests.authored");
    run.wait_marker("review.received");
    assert!(!run.marker("backend.finished").exists());
    assert!(!run.marker("designer.started").exists());
    let state = state(dir.path());
    let session = state["workflows"][state["active"].as_str().unwrap()]["session"]
        .as_str()
        .unwrap();
    let progress: serde_json::Value =
        serde_json::from_slice(&fs::read(Path::new(session).join("team/progress.json")).unwrap())
            .unwrap();
    for role in ["reviewer", "tester", "backend", "frontend"] {
        let task = progress["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["agent"] == role)
            .unwrap();
        assert_eq!(task["status"], "running");
    }
    assert!(
        fs::read_to_string(dir.path().join("tests/contract.md"))
            .unwrap()
            .contains("Contract scenarios")
    );
    run.release("frontend");
    run.wait_marker("designer.started");
    for role in ["backend", "designer", "tester", "reviewer"] {
        run.release(role);
    }
    run.success();
    let progress: serde_json::Value =
        serde_json::from_slice(&fs::read(Path::new(session).join("team/progress.json")).unwrap())
            .unwrap();
    assert!(
        progress["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["status"] == "complete")
    );
}

#[test]
fn independent_backend_designer_and_frontend_run_at_the_same_time() {
    let dir = setup();
    let mut run = Running::spawn(dir.path(), "independent", false);
    for role in ["backend", "designer", "frontend"] {
        run.wait_marker(&format!("{role}.started"));
    }
    // No task can complete until this test releases it; all three have started.
    for role in ["backend", "designer", "frontend"] {
        assert!(!run.marker(&format!("{role}.finished")).exists());
        run.release(role);
    }
    run.success();
}

#[test]
fn invalid_cyclic_plan_is_repaired_before_any_specialist_dispatch() {
    let dir = setup();
    let mut run = Running::spawn(dir.path(), "invalid-first-plan", false);
    for role in ["backend", "designer", "frontend"] {
        run.wait_marker(&format!("{role}.started"));
    }
    assert!(run.marker("repair.approved").exists());
    for role in ["backend", "designer", "frontend"] {
        run.release(role);
    }
    run.success();
    let calls = fs::read_to_string(run.marker("calls")).unwrap();
    assert_eq!(calls.lines().take(2).collect::<Vec<_>>(), ["lead", "lead"]);
    for role in ["backend", "designer", "frontend"] {
        assert_eq!(calls.lines().filter(|called| *called == role).count(), 1);
    }
    let state = state(dir.path());
    let workflow = &state["workflows"][state["active"].as_str().unwrap()];
    assert_eq!(workflow["status"], "complete");
    assert!(!workflow["tasks"].to_string().contains("INVALID_CYCLE"));
}

#[test]
fn cancellation_terminates_every_parallel_cli_process_group() {
    let dir = setup();
    let mut run = Running::spawn(dir.path(), "independent", false);
    let mut pids = Vec::new();
    for role in ["backend", "designer", "frontend"] {
        run.wait_marker(&format!("{role}.started"));
        pids.push(
            fs::read_to_string(run.marker(&format!("{role}.pid")))
                .unwrap()
                .trim()
                .parse::<i32>()
                .unwrap(),
        );
    }
    unsafe {
        libc::kill(run.child.id() as i32, libc::SIGINT);
    }
    run.success();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let alive = pids
            .iter()
            .copied()
            .filter(|pid| unsafe { libc::kill(-*pid, 0) } == 0)
            .collect::<Vec<_>>();
        if alive.is_empty() {
            break;
        }
        if Instant::now() >= deadline {
            // A failed assertion must still clean up all fixture descendants.
            for pid in &alive {
                unsafe {
                    libc::kill(-*pid, libc::SIGKILL);
                }
            }
            panic!("Parallel provider groups survived cancellation: {alive:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let state = state(dir.path());
    let workflow = &state["workflows"][state["active"].as_str().unwrap()];
    let session = Path::new(workflow["session"].as_str().unwrap());
    assert!(
        fs::read_to_string(session.join("status.json"))
            .unwrap()
            .contains("CANCELLED")
    );
    for role in ["backend", "designer", "frontend"] {
        assert!(!run.marker(&format!("{role}.finished")).exists());
    }
}

#[test]
fn configured_parallel_limit_is_observed_even_for_disjoint_scopes() {
    let dir = setup();
    let path = dir.path().join(".ai-lead/team.toml");
    let mut config: toml::Value = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    config["max_parallel_agents"] = toml::Value::Integer(1);
    fs::write(path, toml::to_string_pretty(&config).unwrap()).unwrap();
    let mut run = Running::spawn(dir.path(), "capacity", false);
    for role in ["backend", "designer", "frontend"] {
        run.wait_marker(&format!("{role}.started"));
        run.release(role);
    }
    run.success();
}

#[test]
fn frontend_follows_designer_without_waiting_for_unrelated_backend() {
    let dir = setup();
    let mut run = Running::spawn(dir.path(), "dependency", false);
    run.wait_marker("backend.started");
    run.wait_marker("designer.started");
    assert!(!run.marker("frontend.started").exists());
    run.release("designer");
    run.wait_marker("frontend.started");
    assert!(
        !run.marker("backend.finished").exists(),
        "Scheduler introduced a wave barrier"
    );
    run.release("frontend");
    run.release("backend");
    run.success();
}

#[test]
fn designer_sends_live_handoff_while_all_specialists_are_still_running() {
    let dir = setup();
    let mut run = Running::spawn(dir.path(), "live", false);
    for role in ["backend", "designer", "frontend"] {
        run.wait_marker(&format!("{role}.started"));
        assert!(!run.marker(&format!("{role}.finished")).exists());
    }
    // Frontend only marks itself started after reading the live inbox handoff.
    // Designer has not been allowed to return its final report yet.
    for role in ["backend", "designer", "frontend"] {
        run.release(role);
    }
    run.success();
    let state = state(dir.path());
    let workflow = &state["workflows"][state["active"].as_str().unwrap()];
    let session = Path::new(workflow["session"].as_str().unwrap());
    let lead_prompt = fs::read_to_string(session.join("002-lead/prompt.txt")).unwrap();
    assert!(lead_prompt.contains("LIVE_DESIGN_BEFORE_REPORT"));
    let events = fs::read_to_string(session.join("events.jsonl")).unwrap();
    assert!(events.contains("LIVE_DESIGN_BEFORE_REPORT"));
}

#[test]
fn overlapping_directories_and_legacy_unscoped_tasks_are_serialized() {
    for mode in ["conflict", "legacy"] {
        let dir = setup();
        let mut run = Running::spawn(dir.path(), mode, false);
        run.wait_marker("backend.started");
        assert!(!run.marker("frontend.started").exists());
        run.release("backend");
        run.wait_marker("frontend.started");
        assert!(run.marker("backend.finished").exists());
        run.release("frontend");
        run.success();
    }
}

#[test]
fn failed_prerequisite_skips_consumer_then_lead_can_recover_both() {
    let dir = setup();
    let mut run = Running::spawn(dir.path(), "recovery", false);
    run.success();
    let calls = fs::read_to_string(run.marker("calls")).unwrap();
    assert_eq!(calls.lines().filter(|role| *role == "backend").count(), 2);
    assert_eq!(
        calls.lines().filter(|role| *role == "frontend").count(),
        1,
        "Consumer ran with failed prerequisites"
    );
    let state = state(dir.path());
    let workflow = &state["workflows"][state["active"].as_str().unwrap()];
    assert_eq!(workflow["status"], "complete");
    let tasks = workflow["tasks"].as_array().unwrap();
    assert!(
        tasks
            .iter()
            .any(|task| task["agent"] == "frontend" && task["status"] == "skipped")
    );
    assert_eq!(workflow["failures"], serde_json::json!([]));
}

#[test]
fn environment_verification_limits_deliver_without_claiming_verified_success() {
    let dir = setup();
    let mut run = Running::spawn(dir.path(), "limits", false);
    run.success();
    let state = state(dir.path());
    let workflow = &state["workflows"][state["active"].as_str().unwrap()];
    assert_eq!(workflow["status"], "delivered_with_limits");
    let status: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(Path::new(workflow["session"].as_str().unwrap()).join("status.json"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(status["status"], "DELIVERED_WITH_LIMITS");
    assert!(
        workflow
            .to_string()
            .contains("responsive behavior was not verified")
    );
    assert!(!run.marker("backend.started").exists());
}

#[test]
fn resume_preserves_tasks_completed_out_of_assignment_order() {
    let dir = setup();
    let mut first = Running::spawn(dir.path(), "resume", false);
    for role in ["backend", "designer", "frontend"] {
        first.wait_marker(&format!("{role}.started"));
    }
    first.release("designer");
    first.release("frontend");
    // Wait for authoritative checkpoints, rather than the fixture finishing its stdout.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let state = state(dir.path());
        let workflow = &state["workflows"][state["active"].as_str().unwrap()];
        let done = workflow["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|task| {
                (task["agent"] == "designer" || task["agent"] == "frontend")
                    && task["status"] == "complete"
            })
            .count();
        if done == 2 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Out of order reports were not checkpointed: {}",
            first.logs()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!first.marker("backend.finished").exists());
    first.terminate();
    fs::remove_file(first.marker("backend.started")).unwrap();
    let mut resumed = Running::spawn(dir.path(), "resume", true);
    resumed.wait_marker("backend.started");
    resumed.release("backend");
    resumed.success();
    let calls = fs::read_to_string(resumed.marker("calls")).unwrap();
    assert_eq!(calls.lines().filter(|role| *role == "backend").count(), 2);
    for role in ["designer", "frontend"] {
        assert_eq!(
            calls.lines().filter(|called| *called == role).count(),
            1,
            "Completed {role} repeated on resume"
        );
    }
}
