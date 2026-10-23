//! Durable user input; only the coordinator updates workflow memory.
use crate::{
    engine::{Run, route},
    events::Event,
    memory::{Store, Workflow},
    protocol::Message,
};
use anyhow::{Result, bail};
use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    path::Path,
};
use tokio::sync::mpsc::UnboundedSender;

pub fn send(directory: &Path, lead: &str, text: &str) -> Result<()> {
    let path = directory.join("team/user.jsonl");
    crate::team::publish(&path, &[lead.to_owned()], lead, text)?;
    fs::OpenOptions::new()
        .append(true)
        .open(path)?
        .sync_data()?;
    Ok(())
}

pub fn pending(w: &Workflow) -> bool {
    w.team_sessions.iter().any(|session| {
        let path = session.join("team/user.jsonl");
        let offset = *w
            .message_offsets
            .get(&path.to_string_lossy().into_owned())
            .unwrap_or(&0);
        fs::metadata(path).is_ok_and(|m| m.len() > offset as u64)
    })
}

impl Run {
    pub(crate) fn current_config(&self) -> crate::config::Config {
        self.live_config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub(crate) fn has_pending_user_messages(&self, w: &Workflow) -> bool {
        pending(w)
    }

    pub(crate) fn drain_user_messages(
        &self,
        store: &mut Store,
        w: &mut Workflow,
        tx: &UnboundedSender<Event>,
    ) -> Result<bool> {
        let mut changed = false;
        for session in w.team_sessions.clone() {
            if !session.starts_with(self.project.join(".ai-lead/runs")) {
                bail!("Invalid user message session location");
            }
            let path = session.join("team/user.jsonl");
            let mut file = match fs::File::open(&path) {
                Ok(file) => file,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            let key = path.to_string_lossy().into_owned();
            let offset = *w.message_offsets.get(&key).unwrap_or(&0);
            file.seek(SeekFrom::Start(offset as u64))?;
            let mut bytes = Vec::new();
            file.take(256 * 1024).read_to_end(&mut bytes)?;
            let Some(last) = bytes.iter().rposition(|b| *b == b'\n') else {
                continue;
            };
            for line in bytes[..=last]
                .split(|b| *b == b'\n')
                .filter(|line| !line.is_empty())
            {
                let message: Message = serde_json::from_slice(line)?;
                if message.to != self.config.leader.name {
                    bail!("User input must target the lead");
                }
                crate::protocol::validate_messages(std::slice::from_ref(&message), &self.config)?;
                w.transcript.push(
                    serde_json::json!({"round":w.round,"from":"user","message":message.body}),
                );
                route("user", &[message], &mut w.inbox, tx);
            }
            w.message_offsets.insert(key, offset + last + 1);
            changed = true;
        }
        if changed {
            w.round_limit = w
                .round_limit
                .max(w.round.saturating_add(self.config.max_rounds));
            crate::events::emit(
                tx,
                "round_budget",
                "system",
                None,
                w.round_limit.to_string(),
            );
            self.sync_inboxes(w)?;
            store.checkpoint(w)?;
        }
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn queued_input_survives_completed_workflow_and_is_checkpointed_without_duplicate_delivery() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::config::Config::load(
            &crate::config::init(dir.path(), crate::config::Provider::Codex).unwrap(),
        )
        .unwrap();
        let directory = dir.path().join(".ai-lead/runs/session");
        fs::create_dir_all(directory.join("team/inbox")).unwrap();
        let run = Run {
            live_config: std::sync::Arc::new(std::sync::RwLock::new(config.clone())),
            config,
            project: dir.path().into(),
            directory: directory.clone(),
            goal: "Original task".into(),
            demo: false,
            write: false,
            resume: None,
        };
        let mut store = Store::load(dir.path(), false).unwrap();
        let mut w = store.start("session", &run.goal, false, &directory, &run.config);
        w.team_sessions.push(directory.clone());
        w.status = "complete".into();
        store.checkpoint(&w).unwrap();
        assert!(store.pending(None).is_none());
        send(&directory, "lead", "Добавь настройки\nи тёмную тему").unwrap();
        let mut w = Store::load(dir.path(), false)
            .unwrap()
            .pending(None)
            .unwrap();
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        assert!(run.drain_user_messages(&mut store, &mut w, &tx).unwrap());
        assert_eq!(w.inbox["lead"][0]["from"], "user");
        assert_eq!(
            w.inbox["lead"][0]["body"],
            "Добавь настройки\nи тёмную тему"
        );
        assert!(!run.drain_user_messages(&mut store, &mut w, &tx).unwrap());
        let mut restored =
            Store::load(dir.path(), false).unwrap().state.workflows["session"].clone();
        assert!(
            !run.drain_user_messages(&mut store, &mut restored, &tx)
                .unwrap()
        );
        assert_eq!(restored.transcript.len(), 1);
        assert_eq!(restored.inbox["lead"].len(), 1);
    }
}
