use crate::{
    config::{Agent, Provider},
    events::{Event, clip, emit},
    protocol,
};
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::{path::Path, process::Stdio};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
    sync::mpsc::UnboundedSender,
};

pub fn command(
    agent: &Agent,
    leader: bool,
    write: bool,
    schema_path: &Path,
    result_path: &Path,
) -> Result<Command> {
    let mut cmd = match agent.provider {
        Provider::Codex => {
            let mut c = Command::new("codex");
            c.args([
                "exec",
                "--json",
                "--color",
                "never",
                "--skip-git-repo-check",
                "--ephemeral",
            ]);
            if agent.full_access {
                c.arg("--dangerously-bypass-approvals-and-sandbox");
            } else {
                c.arg("--sandbox").arg(if write && !leader {
                    "workspace-write"
                } else {
                    "read-only"
                });
            }
            c.arg("--output-schema")
                .arg(schema_path)
                .arg("--output-last-message")
                .arg(result_path);
            if let Some(effort) = &agent.reasoning {
                c.arg("-c")
                    .arg(format!("model_reasoning_effort=\"{effort}\""));
            }
            c
        }
        Provider::Claude => {
            let mut c = Command::new("claude");
            c.args([
                "-p",
                "--output-format",
                "stream-json",
                "--include-partial-messages",
                "--verbose",
            ]);
            if agent.full_access {
                c.args(["--dangerously-skip-permissions", "--tools", "default"]);
            } else {
                let tools = if write && !leader {
                    "Read,Glob,Grep,Edit,Write"
                } else {
                    "Read,Glob,Grep"
                };
                c.args([
                    "--permission-mode",
                    "dontAsk",
                    "--tools",
                    tools,
                    "--allowedTools",
                    tools,
                ]);
            }
            c.arg("--json-schema")
                .arg(std::fs::read_to_string(schema_path)?);
            c
        }
        _ => bail!("API provider cannot be invoked as a CLI"),
    };
    if let Some(model) = &agent.model {
        cmd.arg("--model").arg(model);
    }
    cmd.args(&agent.extra_args);
    if agent.provider == Provider::Codex {
        cmd.arg("-");
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    Ok(cmd)
}

// A cancelled task must not leave shell/tool descendants running in the background.
#[cfg(unix)]
pub(crate) struct ProcessGroup(pub u32);
#[cfg(unix)]
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        // All descendants start in a group created for this invocation, never our own.
        unsafe {
            libc::kill(-(self.0 as i32), libc::SIGKILL);
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn invoke(
    agent: &Agent,
    leader: bool,
    write: bool,
    project: &Path,
    turn_dir: &Path,
    prompt: &str,
    timeout: u64,
    tx: &UnboundedSender<Event>,
) -> Result<String> {
    tokio::fs::create_dir_all(turn_dir).await?;
    let schema_path = turn_dir.join("schema.json");
    let result_path = turn_dir.join("result.json");
    tokio::fs::write(
        &schema_path,
        serde_json::to_vec_pretty(&protocol::schema(leader))?,
    )
    .await?;
    tokio::fs::write(turn_dir.join("prompt.txt"), prompt).await?;
    if agent.provider.is_api() {
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout),
            crate::api::invoke(agent, leader, write, project, prompt, tx),
        )
        .await
        .context("API provider timed out")??;
        tokio::fs::write(&result_path, &result).await?;
        return Ok(result);
    }
    let mut cmd = command(agent, leader, write, &schema_path, &result_path)?;
    cmd.current_dir(project);
    let mut child = cmd.spawn().with_context(|| {
        format!(
            "Cannot start {:?}; install its CLI and sign in first",
            agent.provider
        )
    })?;
    #[cfg(unix)]
    let _group = ProcessGroup(child.id().context("Child has no process ID")?);
    let mut stdin = child.stdin.take().context("Missing provider stdin")?;
    let stdout = child.stdout.take().context("Missing provider stdout")?;
    let stderr = child.stderr.take().context("Missing provider stderr")?;
    let operation = async {
        let input = async {
            stdin.write_all(prompt.as_bytes()).await?;
            stdin.shutdown().await?;
            drop(stdin);
            Ok::<_, anyhow::Error>(())
        };
        let output = async {
            let mut raw = tokio::fs::File::create(turn_dir.join("stdout.jsonl")).await?;
            let mut lines = BufReader::new(stdout).lines();
            let mut final_response = None;
            let mut failure = None;
            while let Some(line) = lines.next_line().await? {
                raw.write_all(line.as_bytes()).await?;
                raw.write_all(b"\n").await?;
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    if let Some(text) = progress(&v) {
                        let kind = if v["type"] == "stream_event"
                            && v["event"]["delta"]["type"] == "text_delta"
                        {
                            "delta"
                        } else {
                            "output"
                        };
                        emit(tx, kind, &agent.name, None, clip(&text, 16_000));
                    }
                    if v["type"] == "result" {
                        if v["is_error"] == true
                            || v["subtype"]
                                .as_str()
                                .is_some_and(|s| s.starts_with("error"))
                        {
                            failure = Some(clip(&line, 4000));
                        } else if let Some(structured) =
                            v.get("structured_output").filter(|s| !s.is_null())
                        {
                            final_response = Some(serde_json::to_string(structured)?);
                        } else if let Some(text) = v["result"].as_str() {
                            final_response = Some(text.to_owned());
                        }
                    }
                    if v["type"] == "error" || v["type"] == "turn.failed" {
                        failure = Some(clip(&line, 4000));
                    }
                }
            }
            raw.flush().await?;
            Ok::<_, anyhow::Error>((final_response, failure))
        };
        let errors = async {
            let mut raw = tokio::fs::File::create(turn_dir.join("stderr.log")).await?;
            let mut lines = BufReader::new(stderr).lines();
            let mut tail = String::new();
            while let Some(line) = lines.next_line().await? {
                raw.write_all(line.as_bytes()).await?;
                raw.write_all(b"\n").await?;
                tail = clip(&line, 2000);
                emit(tx, "output", &agent.name, None, &tail);
            }
            raw.flush().await?;
            Ok::<_, anyhow::Error>(tail)
        };
        let (_, (response, failure), stderr, status) =
            tokio::try_join!(input, output, errors, async {
                Ok::<_, anyhow::Error>(child.wait().await?)
            })?;
        if !status.success() {
            bail!("Provider exited with {status}: {stderr}");
        }
        if let Some(failure) = failure {
            bail!("Provider reported failure: {failure}");
        }
        match agent.provider {
            Provider::Codex => Ok(tokio::fs::read_to_string(&result_path)
                .await
                .context("Codex did not produce a final response")?),
            Provider::Claude => {
                let text = response.context("Claude did not produce a final result")?;
                tokio::fs::write(&result_path, &text).await?;
                Ok(text)
            }
            _ => unreachable!("API handled above"),
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(timeout), operation)
        .await
        .context("Provider timed out; process was cancelled")?
}

fn progress(value: &Value) -> Option<String> {
    if value["type"] == "stream_event" {
        if value["event"]["delta"]["type"] == "text_delta" {
            return value["event"]["delta"]["text"].as_str().map(str::to_owned);
        }
        if let Some(name) = value["event"]["content_block"]["name"].as_str() {
            return Some(format!("tool: {name}"));
        }
    }
    if let Some(text) = value["item"]["text"].as_str() {
        return Some(text.to_owned());
    }
    if let Some(command) = value["item"]["command"].as_str() {
        let output = value["item"]["aggregated_output"].as_str().unwrap_or("");
        return Some(if output.is_empty() {
            format!("$ {command}")
        } else {
            format!("$ {command}\n{output}")
        });
    }
    if value["item"]["type"] == "file_change" {
        return Some(format!("files: {}", value["item"]["changes"]));
    }
    if let Some(content) = value["message"]["content"].as_array() {
        let text = content
            .iter()
            .filter_map(|c| {
                c["text"]
                    .as_str()
                    .or(c["name"].as_str())
                    .or(c["content"].as_str())
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !text.is_empty() {
            return Some(text);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn provider_arguments_preserve_model_and_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let p = crate::config::init(dir.path(), Provider::Codex).unwrap();
        let config = crate::config::Config::load(&p).unwrap();
        let mut a = config.leader;
        a.model = Some("custom-model".into());
        a.reasoning = Some("xhigh".into());
        let command = command(&a, true, true, Path::new("s"), Path::new("r")).unwrap();
        let args: Vec<_> = command
            .as_std()
            .get_args()
            .map(|s| s.to_str().unwrap())
            .collect();
        assert!(args.contains(&"read-only"));
        assert!(args.contains(&"custom-model"));
        assert!(args.contains(&"model_reasoning_effort=\"xhigh\""));
        assert_eq!(args.last(), Some(&"-"));
    }
    #[test]
    fn detects_both_provider_progress_formats() {
        assert_eq!(
            progress(&serde_json::json!({"item":{"command":"cargo test"}})).as_deref(),
            Some("$ cargo test")
        );
        assert_eq!(
            progress(&serde_json::json!({"message":{"content":[{"type":"text","text":"hello"}]}}))
                .as_deref(),
            Some("hello")
        );
    }
    #[test]
    fn streamed_text_and_command_results_reach_the_live_log() {
        assert_eq!(progress(&serde_json::json!({"type":"stream_event","event":{"delta":{"type":"text_delta","text":"writing code"}}})).as_deref(),Some("writing code"));
        assert_eq!(progress(&serde_json::json!({"item":{"type":"command_execution","command":"cargo test","aggregated_output":"40 passed"}})).as_deref(),Some("$ cargo test\n40 passed"));
        let dir = tempfile::tempdir().unwrap();
        let config = crate::config::Config::load(
            &crate::config::init(dir.path(), Provider::Claude).unwrap(),
        )
        .unwrap();
        std::fs::write(dir.path().join("schema.json"), "{}").unwrap();
        let command = command(
            &config.leader,
            true,
            false,
            &dir.path().join("schema.json"),
            Path::new("r"),
        )
        .unwrap();
        assert!(
            command
                .as_std()
                .get_args()
                .any(|s| s == "--include-partial-messages")
        );
    }
    #[test]
    fn full_access_removes_lead_restrictions_for_both_cli_providers() {
        let dir = tempfile::tempdir().unwrap();
        let path = crate::config::init(dir.path(), Provider::Codex).unwrap();
        let config = crate::config::Config::load(&path).unwrap();
        std::fs::write(dir.path().join("schema"), "{}").unwrap();
        for provider in [Provider::Codex, Provider::Claude] {
            let mut a = config.leader.clone();
            a.provider = provider.clone();
            a.full_access = true;
            let command = command(
                &a,
                true,
                false,
                &dir.path().join("schema"),
                Path::new("result"),
            )
            .unwrap();
            let args: Vec<_> = command
                .as_std()
                .get_args()
                .map(|s| s.to_string_lossy().into_owned())
                .collect();
            assert!(!args.iter().any(|s| {
                ["read-only", "workspace-write", "dontAsk", "--allowedTools"].contains(&s.as_str())
            }));
            assert!(args.iter().any(|s| s
                == if provider == Provider::Codex {
                    "--dangerously-bypass-approvals-and-sandbox"
                } else {
                    "--dangerously-skip-permissions"
                }));
            if provider == Provider::Claude {
                assert!(args.iter().any(|s| s == "default"));
            }
            assert!(
                a.instructions()
                    .unwrap()
                    .contains("<RUNTIME_PERMISSIONS>FULL ACCESS")
            );
        }
    }
}
