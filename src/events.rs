use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc::UnboundedSender;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Event {
    pub time: u64,
    pub kind: String,
    pub agent: String,
    pub to: Option<String>,
    pub text: String,
}

pub fn emit(
    tx: &UnboundedSender<Event>,
    kind: &str,
    agent: &str,
    to: Option<&str>,
    text: impl Into<String>,
) {
    let _ = tx.send(Event {
        time: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        kind: kind.into(),
        agent: agent.into(),
        to: to.map(str::to_owned),
        text: text.into(),
    });
}

pub fn clip(text: &str, limit: usize) -> String {
    let mut chars = text.chars();
    let mut out: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        out.push('…');
    }
    out
}
