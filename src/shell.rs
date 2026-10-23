//! Explicitly consented local shell tool for direct API agents.
use crate::events::{Event, emit};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::{io::AsyncReadExt, process::Command, sync::mpsc::UnboundedSender};
const OUTPUT_LIMIT: usize = 64 * 1024;

pub async fn execute(
    project: &Path,
    full_access: bool,
    args: &Value,
    agent: &str,
    tx: &UnboundedSender<Event>,
) -> Result<Value> {
    if !full_access {
        bail!("Shell requires explicitly authorized Full access");
    }
    let command = args["command"]
        .as_str()
        .context("command must be a string")?;
    if command.trim().is_empty() {
        bail!("command cannot be empty");
    }
    let seconds = args["timeout_seconds"]
        .as_u64()
        .context("timeout_seconds must be an integer (1..300)")?;
    if !(1..=300).contains(&seconds) {
        bail!("timeout_seconds must be 1..300");
    }
    let cwd = project
        .join(args["cwd"].as_str().context("cwd must be a string")?)
        .canonicalize()?;
    if !cwd.is_dir() {
        bail!("cwd must be a directory");
    }
    #[cfg(unix)]
    let mut process = {
        let mut c = Command::new("/bin/sh");
        c.args(["-c", command]);
        c.process_group(0);
        c
    };
    #[cfg(not(unix))]
    let mut process = {
        let mut c = Command::new("cmd.exe");
        c.args(["/D", "/S", "/C", command]);
        c
    };
    process
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    emit(tx, "output", agent, None, format!("$ {command}"));
    let mut child = process.spawn().context("Cannot start API shell command")?;
    #[cfg(unix)]
    let _group = crate::provider::ProcessGroup(child.id().context("Missing shell process ID")?);
    let stdout = child.stdout.take().context("Missing command stdout")?;
    let stderr = child.stderr.take().context("Missing command stderr")?;
    let operation = async {
        let (stdout, stderr, status) = tokio::try_join!(
            capture(stdout, agent, "stdout", tx),
            capture(stderr, agent, "stderr", tx),
            child.wait()
        )?;
        Ok::<_, anyhow::Error>(
            json!({"exit_code":status.code(),"success":status.success(),"stdout":stdout.0,"stderr":stderr.0,"truncated":stdout.1||stderr.1,"timed_out":false}),
        )
    };
    match tokio::time::timeout(Duration::from_secs(seconds), operation).await {
        Ok(result) => result,
        Err(_) => {
            emit(
                tx,
                "output",
                agent,
                None,
                format!("Command timed out after {seconds}s; stopping its process group"),
            );
            Ok(
                json!({"success":false,"timed_out":true,"error":format!("Command timed out after {seconds}s; streamed partial output is in session events")}),
            )
        }
    }
}
async fn capture(
    mut stream: impl tokio::io::AsyncRead + Unpin,
    agent: &str,
    label: &str,
    tx: &UnboundedSender<Event>,
) -> std::io::Result<(String, bool)> {
    let mut result = Vec::new();
    let mut truncated = false;
    let mut buffer = [0; 8192];
    loop {
        let size = stream.read(&mut buffer).await?;
        if size == 0 {
            break;
        }
        emit(
            tx,
            "output",
            agent,
            None,
            format!("{label}: {}", String::from_utf8_lossy(&buffer[..size])),
        );
        let remaining = OUTPUT_LIMIT.saturating_sub(result.len());
        result.extend_from_slice(&buffer[..size.min(remaining)]);
        truncated |= size > remaining;
    }
    Ok((String::from_utf8_lossy(&result).into_owned(), truncated))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[tokio::test]
    async fn shell_requires_capability_and_reports_real_output_and_exit_status() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let args = json!({"command":"printf hello; printf problem >&2; exit 7","cwd":".","timeout_seconds":5});
        assert!(
            execute(dir.path(), false, &args, "lead", &tx)
                .await
                .is_err()
        );
        let output = execute(dir.path(), true, &args, "lead", &tx).await.unwrap();
        assert_eq!(output["stdout"], "hello");
        assert_eq!(output["stderr"], "problem");
        assert_eq!(output["exit_code"], 7);
        assert_eq!(output["success"], false);
        let mut events = Vec::new();
        while let Ok(e) = rx.try_recv() {
            events.push(e.text);
        }
        assert!(events.iter().any(|s| s.contains("stdout: hello")));
    }

    #[tokio::test]
    async fn cancellation_stops_shell_descendants() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_owned();
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            execute(&root,true,&json!({"command":"printf started > started.txt; sleep 2; printf leaked > cancelled.txt","cwd":".","timeout_seconds":5}),"backend",&tx).await
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while !dir.path().join("started.txt").exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        task.abort();
        let _ = task.await;
        tokio::time::sleep(Duration::from_millis(2200)).await;
        assert!(!dir.path().join("cancelled.txt").exists());
    }
    #[tokio::test]
    async fn shell_timeout_kills_descendants() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        let args =
            json!({"command":"sleep 2; printf leaked > late.txt","cwd":".","timeout_seconds":1});
        assert_eq!(
            execute(dir.path(), true, &args, "tester", &tx)
                .await
                .unwrap()["timed_out"],
            true
        );
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert!(!dir.path().join("late.txt").exists());
    }
}
