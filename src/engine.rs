use crate::{
    config::Config,
    events::{Event, clip, emit},
    memory::{Store, Workflow},
    protocol::{self, Decision, Report},
    provider,
};
use anyhow::{Context, Result, bail};
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
use tokio::sync::mpsc::UnboundedSender;

#[derive(Clone)]
pub struct Run {
    pub live_config: std::sync::Arc<std::sync::RwLock<Config>>,
    pub config: Config,
    pub project: PathBuf,
    pub directory: PathBuf,
    pub goal: String,
    pub demo: bool,
    pub write: bool,
    pub resume: Option<String>,
}
impl Run {
    pub async fn execute(&self, tx: &UnboundedSender<Event>) -> Result<String> {
        let mut store = Store::load(&self.project, self.demo)?;
        let mut workflow = if let Some(id) = &self.resume {
            let mut w = store
                .state
                .workflows
                .get(id)
                .cloned()
                .context("No workflow with that ID")?;
            if !w.unfinished() && !self.has_pending_user_messages(&w) {
                bail!("Completed workflow has no new user message");
            }
            if w.goal != self.goal || w.write != self.write {
                bail!("Resume metadata changed; restart from the saved goal and access mode");
            }
            if w.plan.as_ref().is_some_and(|d| d.action != "delegate") {
                w.plan = None;
                w.round += 1;
            }
            w.session = self.directory.clone();
            w.status = "resuming".into();
            w
        } else {
            let id = self
                .directory
                .file_name()
                .context("Missing session ID")?
                .to_string_lossy();
            store.start(&id, &self.goal, self.write, &self.directory, &self.config)
        };
        if !workflow.team_sessions.contains(&self.directory) {
            workflow.team_sessions.push(self.directory.clone());
        }
        fs::create_dir_all(self.directory.join("team/inbox"))?;
        fs::create_dir_all(self.directory.join("team/outbox"))?;
        self.sync_inboxes(&workflow)?;
        store.checkpoint(&workflow)?;
        emit(
            tx,
            "session",
            "system",
            None,
            format!(
                "{} · {} · {}",
                if self.demo {
                    "DEMO — no models called"
                } else {
                    "LIVE"
                },
                if self.config.leader.full_access {
                    "FULL ACCESS — all agents"
                } else if self.write {
                    "workspace-write"
                } else {
                    "read-only"
                },
                self.directory.display()
            ),
        );
        emit(
            tx,
            "output",
            "system",
            None,
            format!(
                "Persistent memory: {}. Workflow: {}{}",
                store.root.display(),
                workflow.id,
                if self.resume.is_some() {
                    " (resumed)"
                } else {
                    ""
                }
            ),
        );
        for name in store.state.agents.keys() {
            emit_map(&store, name, tx)?;
        }
        let result = self.execute_workflow(&mut store, &mut workflow, tx).await;
        if let Err(error) = &result {
            if workflow.status != "blocked" {
                workflow.status = "interrupted".into();
            }
            workflow.last_error = Some(format!("{error:#}"));
            store.checkpoint(&workflow)?;
        }
        result
    }
    async fn execute_workflow(
        &self,
        store: &mut Store,
        w: &mut Workflow,
        tx: &UnboundedSender<Event>,
    ) -> Result<String> {
        // Follow-ups extend a bounded budget; persist it so an interrupted
        // continuation can resume beyond the initial workflow's round limit.
        w.round_limit = w.round_limit.max(self.config.max_rounds);
        self.drain_user_messages(store, w, tx)?;
        emit(
            tx,
            "round_budget",
            "system",
            None,
            w.round_limit.to_string(),
        );
        store.checkpoint(w)?;
        while w.round <= w.round_limit {
            self.drain_user_messages(store, w, tx)?;
            let config = self.current_config();
            let roster=config.agents.iter().map(|a|Ok(json!({"name":a.name,"provider":a.provider,"model":a.model,"role_prompt":a.role_prompt()?}))).collect::<Result<Vec<_>>>()?;
            let round = w.round;
            emit(
                tx,
                "round",
                &self.config.leader.name,
                None,
                round.to_string(),
            );
            if w.plan.is_none() {
                let id = format!("{round:03}-lead");
                let task = w.task(
                    &id,
                    &self.config.leader.name,
                    "Delegate, coordinate and plan final launch",
                );
                task.status = "running".into();
                task.attempts += 1;
                w.status = "planning".into();
                store.checkpoint(w)?;
                emit(
                    tx,
                    "working",
                    &self.config.leader.name,
                    None,
                    "Planning next action from persistent team memory",
                );
                let mut prompt = format!(
                    "{}\n\nYou are {}. Specialist roster:\n{}\n\nGoal:\n{}\n\nRound {round}/{}. Workspace mode: {}. Independent assignments execute concurrently with file ownership and dependency constraints. Provide a shared brief with stack, provisional architecture, API contracts and a joint designer/frontend UI objective. Route backend work to backend and UI work to designer/frontend. files=null reserves the whole project; disjoint file/directory prefixes permit concurrency, [] means read-only. depends_on names agents in this same plan. Do not make frontend wait for backend when an agreed contract and fixtures suffice. Read all reports and messages. You alone decide delegation, fixes and completion. Use action=blocked for missing implementation or real defects. For implemented work with only environmental verification gaps, use finish_with_limits and list verification_limits; never claim those checks passed. Never finish with unresolved specialist failures. Reassign failed agents or stop as blocked. STRICT ROLES: You only delegate, coordinate and launch the delivered project at the end; write no project code or tests. Architect chooses technical architecture. Tester alone writes tests and runs tests/builds/browser checks, starting alongside implementation. Reviewer performs read-only logic review during implementation (files=[]), sends live findings and never patches code. Backend/frontend never write tests. Frontend also implements mobile/desktop/terminal applications as required. Schedule early QA/review passes and current final passes after fixes; ask authors to publish inspectable paths immediately. Return JSON matching:\n{}\n\nRecent team transcript (full history is in persistent state and {}):\n{}\n\nYour inbox:\n{}{}",
                    config.leader.instructions()?,
                    self.config.leader.name,
                    serde_json::to_string(&roster)?,
                    self.goal,
                    self.config.max_rounds,
                    if self.config.leader.full_access {
                        "FULL ACCESS for all agents including the lead; local shell and filesystem access permitted within the user's task"
                    } else if self.write {
                        "workspace-write specialists; lead read-only"
                    } else {
                        "read-only; findings/specifications only"
                    },
                    protocol::schema(true),
                    self.directory.display(),
                    recent(&w.transcript),
                    serde_json::to_string(
                        &w.inbox
                            .get(&self.config.leader.name)
                            .cloned()
                            .unwrap_or_default()
                    )?,
                    store.context(&self.config.leader.name, w)?
                );
                prompt.push_str("\n\nFOLLOW-UP USER INPUT: Messages attributed to user in your inbox/transcript are direct updates from the user, not teammate suggestions. Incorporate them into the current goal and distribute resulting work only to the proper roles. A follow-up after delivery continues this project with the saved context. Respond with a fresh plan for new requested work, not a repetition of the prior completion summary.\n");
                let decision: Decision = if self.demo {
                    self.demo_decision(round)
                } else {
                    let mut accepted = None;
                    let mut last_error = String::new();
                    for attempt in 0..3 {
                        if attempt > 0 {
                            w.task(
                                &id,
                                &self.config.leader.name,
                                "Delegate, coordinate and plan final launch",
                            )
                            .attempts += 1;
                            store.checkpoint(w)?;
                        }
                        let turn = if attempt == 0 {
                            self.directory.join(&id)
                        } else {
                            self.directory.join(format!("{id}-repair-{attempt}"))
                        };
                        let raw = provider::invoke(
                            &config.leader,
                            true,
                            false,
                            &self.project,
                            &turn,
                            &prompt,
                            self.config.timeout_seconds,
                            tx,
                        )
                        .await?;
                        let checked = protocol::parse::<Decision>(&raw).and_then(|mut d| {
                            d.normalize_roles();
                            d.validate(&self.config)?;
                            Ok(d)
                        });
                        match checked {
                            Ok(decision) => {
                                accepted = Some(decision);
                                break;
                            }
                            Err(error) => {
                                last_error = format!("{error:#}");
                                emit(
                                    tx,
                                    "output",
                                    &self.config.leader.name,
                                    None,
                                    format!(
                                        "Plan rejected before dispatch: {last_error}. Validation attempt {}/3",
                                        attempt + 1
                                    ),
                                );
                                prompt.push_str(&format!("\n\nCOORDINATOR FEEDBACK: Your previous plan was rejected and NO specialist task from it was dispatched: {last_error}. Correct the structured response using only configured agent names, an acyclic dependency graph and valid non-overlapping ownership. Return the corrected full decision. Previous response: {}",clip(&raw,16000)));
                            }
                        }
                    }
                    accepted.with_context(|| {
                        format!(
                            "Lead could not produce a valid plan after 3 attempts: {last_error}"
                        )
                    })?
                };
                // Input arriving during inference must be read before dispatch/closure.
                // The current provider call is allowed to finish; its stale plan is discarded.
                if self.drain_user_messages(store, w, tx)? {
                    let task = w.task(&id, &self.config.leader.name, "Replan for new user input");
                    task.status = "superseded".into();
                    task.summary = Some(
                        "New user input arrived during planning; no assignments dispatched".into(),
                    );
                    emit(
                        tx,
                        "output",
                        &self.config.leader.name,
                        None,
                        "New user input received; updating the plan before dispatch",
                    );
                    w.round += 1;
                    store.checkpoint(w)?;
                    continue;
                }
                decision.validate(&self.config)?;
                w.inbox.remove(&self.config.leader.name);
                w.transcript.push(
                    json!({"round":round,"from":self.config.leader.name,"decision":decision}),
                );
                let task = w.task(
                    &id,
                    &self.config.leader.name,
                    "Delegate, coordinate and plan final launch",
                );
                task.status = "complete".into();
                task.summary = Some(decision.summary.clone());
                store.record(
                    w,
                    &self.config.leader.name,
                    &id,
                    "Delegate, coordinate and plan final launch",
                    &decision.summary,
                    "complete",
                    &decision.memory,
                );
                route(
                    &self.config.leader.name,
                    &decision.messages,
                    &mut w.inbox,
                    tx,
                );
                for (index, a) in decision.assignments.iter().enumerate() {
                    w.task(
                        &format!("{round:03}-{index:03}-{}", a.agent),
                        &a.agent,
                        &a.task,
                    );
                }
                w.plan = Some(decision.clone());
                w.cursor = 0;
                store.checkpoint(w)?;
                persist(&self.directory, w).await?;
                emit(
                    tx,
                    "report",
                    &self.config.leader.name,
                    None,
                    &decision.summary,
                );
                emit_map(store, &self.config.leader.name, tx)?;
            }
            let decision = w.plan.clone().context("Missing checkpointed decision")?;
            decision.validate(&self.config)?;
            if decision.action == "blocked" {
                w.status = "blocked".into();
                bail!("Lead stopped the incomplete task: {}", decision.summary);
            }
            if decision.action == "finish" || decision.action == "finish_with_limits" {
                if !w.failures.is_empty() {
                    bail!(
                        "Cannot approve completion with unresolved specialist failures: {:?}",
                        w.failures
                    );
                }
                let limited = decision.action == "finish_with_limits";
                w.status = if limited {
                    "delivered_with_limits"
                } else {
                    "complete"
                }
                .into();
                w.last_error = None;
                store.checkpoint(w)?;
                persist(&self.directory, w).await?;
                let summary = if limited {
                    format!(
                        "{}\n\nVerification still required:\n{}",
                        decision.summary,
                        decision
                            .verification_limits
                            .iter()
                            .map(|s| format!("- {s}"))
                            .collect::<Vec<_>>()
                            .join("\n")
                    )
                } else {
                    decision.summary
                };
                emit(
                    tx,
                    if limited { "done_with_limits" } else { "done" },
                    &self.config.leader.name,
                    None,
                    &summary,
                );
                return Ok(summary);
            }
            emit(
                tx,
                "waiting",
                &self.config.leader.name,
                None,
                "Awaiting specialist reports",
            );
            self.execute_assignments(store, w, &decision, tx).await?;
            w.plan = None;
            w.cursor = 0;
            w.round += 1;
            w.status = "planning".into();
            store.checkpoint(w)?;
        }
        bail!(
            "Round limit reached without leader approval. Increase max_rounds and use resume; the task is incomplete."
        )
    }

    fn demo_decision(&self, round: usize) -> Decision {
        Decision {brief:"[SIMULATED] API contract agreed up front. Backend and UI implement independently; designer owns design tokens and frontend owns page composition.".into(),verification_limits:vec![],action:if round==1{"delegate"}else{"finish"}.into(),assignments:if round==1{self.config.agents.iter().map(|a|protocol::Assignment{agent:a.name.clone(),task:format!("Inspect the {} scope and report acceptance criteria for the goal.",a.name),depends_on:vec![],files:Some(if a.name=="reviewer"{vec![]}else{vec![format!("demo/{}",a.name)]})}).collect()}else{vec![]},
            messages:vec![],summary:if round==1{"DEMO: prepare the team and coordinate design, implementation, testing and review."}else{"DEMO complete. Reports, peer messages and memory were saved. No provider was called and no real checks were run."}.into(),memory:Default::default()}
    }
    pub(crate) fn demo_report(&self, name: &str) -> Report {
        let other = self
            .config
            .agents
            .iter()
            .find(|a| a.name != name)
            .map(|a| a.name.clone());
        Report {verification_limits:vec![],summary:format!("[SIMULATED] {name}: scope inspected, handoff prepared. No files changed or checks executed."),messages:other.map(|to|vec![protocol::Message{to,body:format!("[SIMULATED] Handoff from {name}: coordinate acceptance criteria before implementation.")}]).unwrap_or_default(),
            memory:protocol::MemoryUpdate{facts:vec![format!("[SIMULATED] {name} prepared a handoff")],shared:vec![format!("[SIMULATED] {name} reviewed scope")],..Default::default()}}
    }
}
pub(crate) fn emit_map(store: &Store, name: &str, tx: &UnboundedSender<Event>) -> Result<()> {
    emit(
        tx,
        "memory",
        name,
        None,
        serde_json::to_string_pretty(&store.agent_map(name))?,
    );
    emit(
        tx,
        "shared_memory",
        "system",
        None,
        serde_json::to_string_pretty(&store.state.shared)?,
    );
    Ok(())
}
pub(crate) fn route(
    from: &str,
    messages: &[protocol::Message],
    inbox: &mut BTreeMap<String, Vec<serde_json::Value>>,
    tx: &UnboundedSender<Event>,
) {
    for message in messages {
        inbox
            .entry(message.to.clone())
            .or_default()
            .push(json!({"from":from,"body":message.body}));
        emit(tx, "message", from, Some(&message.to), &message.body);
    }
}
pub(crate) fn recent(transcript: &[serde_json::Value]) -> String {
    let mut selected = Vec::new();
    let mut size = 0;
    for item in transcript.iter().rev() {
        let text = clip(&item.to_string(), 16000);
        size += text.len();
        if size > 96000 {
            break;
        }
        selected.push(text);
    }
    selected.reverse();
    selected.join("\n")
}
pub(crate) async fn persist(directory: &Path, w: &Workflow) -> Result<()> {
    tokio::fs::write(
        directory.join("transcript.json"),
        serde_json::to_vec_pretty(&w.transcript)?,
    )
    .await?;
    tokio::fs::write(
        directory.join("inbox.json"),
        serde_json::to_vec_pretty(&w.inbox)?,
    )
    .await?;
    Ok(())
}
pub fn lock(project: &Path) -> Result<std::fs::File> {
    std::fs::create_dir_all(project.join(".ai-lead"))?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(project.join(".ai-lead/run.lock"))?;
    file.try_lock()
        .context("Another AI Lead run is using this workspace")?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn workspace_lock_excludes_concurrent_writers() {
        let dir = tempfile::tempdir().unwrap();
        let held = lock(dir.path()).unwrap();
        assert!(lock(dir.path()).is_err());
        drop(held);
        assert!(lock(dir.path()).is_ok());
    }
    #[test]
    fn rejects_unknown_targets_and_contradictory_finish() {
        let dir = tempfile::tempdir().unwrap();
        let config =
            Config::load(&crate::config::init(dir.path(), crate::config::Provider::Codex).unwrap())
                .unwrap();
        let mut decision = Decision {
            brief: String::new(),
            verification_limits: vec![],
            action: "delegate".into(),
            assignments: vec![protocol::Assignment {
                agent: "invented".into(),
                task: "x".into(),
                depends_on: vec![],
                files: None,
            }],
            messages: vec![],
            summary: "Plan".into(),
            memory: Default::default(),
        };
        assert!(decision.validate(&config).is_err());
        decision.assignments[0].agent = "backend".into();
        assert!(decision.validate(&config).is_ok());
        decision.action = "finish".into();
        assert!(decision.validate(&config).is_err());
    }
}
