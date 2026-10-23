//! File tools are confined in restricted mode and unrestricted after Full access consent.
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Component, Path, PathBuf},
};
const LIMIT: u64 = 128 * 1024;

pub fn message_definition(peers: &[String]) -> Value {
    json!({"name":"send_message","description":"Immediately send a concise task update, interface contract, design decision, question or blocker to another teammate while both tasks run. Do not wait for submit_result. The lead receives every message. Available even in read-only mode.","parameters":{"type":"object","properties":{"to":{"type":"string","enum":peers},"body":{"type":"string","minLength":1,"maxLength":16384}},"required":["to","body"],"additionalProperties":false}})
}

pub fn definitions(write: bool, full_access: bool, schema: Value) -> Vec<Value> {
    let mut tools = vec![
        json!({"name":"list_files","description":"List immediate directory entries. Paths are relative to the project.","parameters":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false}}),
        json!({"name":"read_file","description":"Read a UTF-8 project file (up to 128 KiB).","parameters":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false}}),
    ];
    if write {
        tools.push(json!({"name":"write_file","description":"Create or replace a UTF-8 project file. Read existing files before editing. Maximum 128 KiB.","parameters":{"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"],"additionalProperties":false}}));
    }
    if full_access {
        tools.push(json!({"name":"run_command","description":"Run a shell command with your OS account's full permissions. stdout/stderr stream live; the returned output is capped at 64 KiB per stream. cwd can be absolute or relative to the project. No interactive stdin. Stay within your assigned task.","parameters":{"type":"object","properties":{"command":{"type":"string"},"cwd":{"type":"string"},"timeout_seconds":{"type":"integer","minimum":1,"maximum":300}},"required":["command","cwd","timeout_seconds"],"additionalProperties":false}}));
        for tool in &mut tools {
            if tool["name"] != "run_command" {
                let description=tool["description"].as_str().unwrap_or_default().replace("Paths are relative to the project.","Paths can be absolute or relative to the project; access outside the project is permitted for your task.");
                tool["description"] = json!(format!(
                    "{description} Full access: absolute paths and paths outside the project are allowed."
                ));
            }
        }
    }
    tools.push(json!({"name":"submit_result","description":"Finish your turn with the requested structured result, messages and memory updates.","parameters":schema}));
    tools
}

fn path(root: &Path, input: &str, full_access: bool) -> Result<PathBuf> {
    if full_access {
        return Ok(root.join(input));
    }
    let relative = Path::new(input);
    if relative
        .components()
        .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        bail!("Only project-relative paths are allowed");
    }
    for c in relative.components() {
        if let Component::Normal(name) = c {
            let name = name.to_string_lossy();
            if name == ".git" || name == ".ai-lead" || name == ".env" || name.starts_with(".env.") {
                bail!("Protected path");
            }
        }
    }
    let root = root.canonicalize()?;
    let mut current = root.clone();
    for c in relative.components() {
        current.push(c);
        if let Ok(meta) = fs::symlink_metadata(&current) {
            if meta.file_type().is_symlink() {
                bail!("Symlinks are not supported by API file tools");
            }
            if !current.canonicalize()?.starts_with(&root) {
                bail!("Path escapes project");
            }
        }
    }
    Ok(current)
}

#[cfg(test)]
pub fn execute(root: &Path, write: bool, name: &str, args: &Value) -> Result<Value> {
    execute_with_access(root, write, false, name, args)
}
pub fn execute_with_access(
    root: &Path,
    write: bool,
    full_access: bool,
    name: &str,
    args: &Value,
) -> Result<Value> {
    if !matches!(name, "read_file" | "list_files" | "write_file") {
        bail!("Unknown tool: {name}");
    }
    if name == "write_file" && !write && !full_access {
        bail!("This agent is read-only");
    }
    let input = args["path"].as_str().context("path must be a string")?;
    let file = path(root, input, full_access)?;
    match name {
        "read_file" => {
            let meta = fs::metadata(&file)?;
            if !meta.is_file() || meta.len() > LIMIT {
                bail!("Expected a regular file up to 128 KiB");
            }
            Ok(json!({"content":fs::read_to_string(file)?}))
        }
        "list_files" => {
            let mut names = Vec::new();
            let mut truncated = false;
            for (index, entry) in fs::read_dir(file)?.take(1001).enumerate() {
                if index == 1000 {
                    truncated = true;
                    break;
                }
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().to_string();
                if path(
                    root,
                    &Path::new(input).join(&name).to_string_lossy(),
                    full_access,
                )
                .is_err()
                {
                    continue;
                }
                names.push(format!(
                    "{name}{}",
                    if entry.file_type()?.is_dir() { "/" } else { "" }
                ));
            }
            names.sort();
            Ok(json!({"entries":names,"truncated":truncated}))
        }
        "write_file" => {
            let content = args["content"]
                .as_str()
                .context("content must be a string")?;
            if content.len() as u64 > LIMIT {
                bail!("File exceeds 128 KiB");
            }
            if file.is_dir() {
                bail!("Cannot replace a directory");
            }
            fs::create_dir_all(file.parent().context("Missing parent")?)?;
            // Replace atomically so cancellation cannot leave a half-written file.
            let temporary = file
                .parent()
                .unwrap()
                .join(format!(".ai-lead-write-{}", uuid::Uuid::new_v4()));
            fs::write(&temporary, content)?;
            if let Ok(meta) = fs::metadata(&file)
                && let Err(error) = fs::set_permissions(&temporary, meta.permissions())
            {
                let _ = fs::remove_file(&temporary);
                return Err(error.into());
            }
            if let Err(error) = fs::rename(&temporary, &file) {
                let _ = fs::remove_file(&temporary);
                return Err(error.into());
            }
            Ok(json!({"written":input}))
        }
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn permissions_and_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        for p in [
            "../escape",
            "/tmp/escape",
            ".git/config",
            ".ai-lead/team.toml",
            ".env.local",
        ] {
            assert!(
                execute(
                    dir.path(),
                    true,
                    "write_file",
                    &json!({"path":p,"content":"bad"})
                )
                .is_err()
            );
        }
        assert!(
            execute(
                dir.path(),
                false,
                "write_file",
                &json!({"path":"x","content":"bad"})
            )
            .is_err()
        );
        execute(
            dir.path(),
            true,
            "write_file",
            &json!({"path":"src/a.rs","content":"hello"}),
        )
        .unwrap();
        assert_eq!(
            execute(dir.path(), false, "read_file", &json!({"path":"src/a.rs"})).unwrap()["content"],
            "hello"
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("src/a.rs", dir.path().join("link")).unwrap();
            assert!(
                execute(
                    dir.path(),
                    true,
                    "write_file",
                    &json!({"path":"link","content":"bad"})
                )
                .is_err()
            );
        }
    }
}
