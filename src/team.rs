//! Runtime mailboxes shared by CLI and HTTP agents.
//! The coordinator owns inbox snapshots; agents append only to their own outbox.
use crate::protocol::Message;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{fs, io::Write, path::Path};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Incoming {
    pub from: String,
    pub body: String,
}

/// Runtime metadata is delivered separately from project file tools, including
/// restricted API sessions where .ai-lead is intentionally not readable.
pub fn read_progress(inbox: &Path, previous: &mut String) -> Result<Option<String>> {
    let Some(team) = inbox.parent().and_then(Path::parent) else {
        return Ok(None);
    };
    let path = team.join("progress.json");
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).context("Cannot read team progress"),
    };
    use std::io::Read;
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        bail!("Team progress exceeds 1 MiB");
    }
    let progress = String::from_utf8(bytes).context("Invalid team progress encoding")?;
    if progress == *previous {
        return Ok(None);
    }
    serde_json::from_str::<serde_json::Value>(&progress)
        .context("Invalid team progress snapshot")?;
    *previous = progress.clone();
    Ok(Some(progress))
}

/// Read additions to an atomically replaced inbox snapshot.
pub fn read_new(path: &Path, cursor: &mut usize) -> Result<Vec<Incoming>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).context("Cannot read team inbox"),
    };
    if bytes.len() > 4 * 1024 * 1024 {
        bail!("Team inbox exceeds 4 MiB");
    }
    let messages: Vec<Incoming> = serde_json::from_slice(&bytes).context("Invalid team inbox")?;
    // Reset is permitted between tasks; normal snapshots only grow within a task.
    if *cursor > messages.len() {
        *cursor = 0;
    }
    let fresh = messages[*cursor..].to_vec();
    *cursor = messages.len();
    Ok(fresh)
}

/// A complete JSONL frame is appended in a single write. Each agent has one writer.
pub fn publish(path: &Path, peers: &[String], to: &str, body: &str) -> Result<()> {
    if !peers.iter().any(|peer| peer == to) {
        bail!("Unknown team recipient: {to}");
    }
    if body.trim().is_empty() || body.len() > 16 * 1024 {
        bail!("Team message must contain 1..16384 UTF-8 bytes");
    }
    let message = Message {
        to: to.into(),
        body: body.into(),
    };
    let mut frame = serde_json::to_vec(&message)?;
    frame.push(b'\n');
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(&frame)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapshots_only_deliver_new_messages_and_outbox_checks_recipients() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("inbox.json");
        let mut cursor = 0;
        assert!(read_new(&inbox, &mut cursor).unwrap().is_empty());
        fs::write(
            &inbox,
            r#"[{"from":"designer","body":"Use stacked cards"}]"#,
        )
        .unwrap();
        assert_eq!(read_new(&inbox, &mut cursor).unwrap()[0].from, "designer");
        assert!(read_new(&inbox, &mut cursor).unwrap().is_empty());
        fs::write(
            &inbox,
            r#"[{"from":"designer","body":"Use stacked cards"},{"from":"lead","body":"Ship it"}]"#,
        )
        .unwrap();
        assert_eq!(read_new(&inbox, &mut cursor).unwrap()[0].body, "Ship it");
        let outbox = dir.path().join("outbox.jsonl");
        let peers = vec!["frontend".into()];
        assert!(publish(&outbox, &peers, "unknown", "hello").is_err());
        assert!(publish(&outbox, &peers, "frontend", " ").is_err());
        publish(&outbox, &peers, "frontend", "line one\nline two").unwrap();
        let lines = fs::read_to_string(outbox).unwrap();
        assert_eq!(lines.lines().count(), 1);
        assert_eq!(
            serde_json::from_str::<Message>(lines.trim()).unwrap().to,
            "frontend"
        );
    }
}
