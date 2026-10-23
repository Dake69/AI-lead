//! Concurrent dependency scheduler and durable per-task checkpointing.
use crate::{
    engine::{Run, emit_map, persist, recent, route},
    events::{Event, emit},
    memory::{Store, Workflow},
    protocol::{self, Decision, Report},
    provider,
};
use anyhow::{Context, Result, bail};
use serde_json::json;
use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Seek, SeekFrom},
};
use tokio::sync::mpsc::UnboundedSender;

impl Run {
    fn sync_progress(&self, w: &Workflow, decision: &Decision) -> Result<()> {
        let tasks: Vec<_> = decision.assignments.iter().enumerate().map(|(index, a)| {
            let id = format!("{:03}-{index:03}-{}", w.round, a.agent);
            let saved = w.tasks.iter().find(|task| task.id == id);
            json!({"id": id, "agent": a.agent, "task": a.task, "files": a.files,
                "depends_on": a.depends_on, "status": saved.map_or("pending", |task| task.status.as_str()),
                "summary": saved.and_then(|task| task.summary.as_deref())})
        }).collect();
        let path = self.directory.join("team/progress.json");
        let temporary = path.with_extension("tmp");
        fs::write(
            &temporary,
            serde_json::to_vec_pretty(&json!({"round": w.round, "tasks": tasks}))?,
        )?;
        fs::rename(temporary, path)?;
        Ok(())
    }

    fn notify_observers(
        &self,
        w: &mut Workflow,
        from: &str,
        body: String,
        tx: &UnboundedSender<Event>,
    ) {
        let messages: Vec<_> = self
            .config
            .agents
            .iter()
            .filter(|agent| {
                matches!(agent.name.as_str(), "tester" | "reviewer") && agent.name != from
            })
            .map(|agent| protocol::Message {
                to: agent.name.clone(),
                body: body.clone(),
            })
            .collect();
        route(from, &messages, &mut w.inbox, tx);
    }

    pub(crate) fn sync_inboxes(&self, w: &Workflow) -> Result<()> {
        for agent in std::iter::once(&self.config.leader).chain(&self.config.agents) {
            let path = self
                .directory
                .join("team/inbox")
                .join(format!("{}.json", agent.name));
            let temporary = path.with_extension("tmp");
            fs::write(
                &temporary,
                serde_json::to_vec(w.inbox.get(&agent.name).unwrap_or(&vec![]))?,
            )?;
            fs::rename(temporary, path)?;
        }
        Ok(())
    }
    fn drain_messages(
        &self,
        store: &mut Store,
        w: &mut Workflow,
        tx: &UnboundedSender<Event>,
    ) -> Result<()> {
        self.drain_user_messages(store, w, tx)?;
        let mut changed = false;
        for session in w.team_sessions.clone() {
            if !session.starts_with(self.project.join(".ai-lead/runs")) {
                bail!("Invalid team session location");
            }
            for sender in std::iter::once(&self.config.leader).chain(&self.config.agents) {
                let path = session
                    .join("team/outbox")
                    .join(format!("{}.jsonl", sender.name));
                let mut file = match fs::File::open(&path) {
                    Ok(f) => f,
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
                    .filter(|l| !l.is_empty())
                {
                    let result = serde_json::from_slice::<protocol::Message>(line)
                        .map_err(anyhow::Error::from)
                        .and_then(|m| {
                            protocol::validate_messages(std::slice::from_ref(&m), &self.config)?;
                            Ok(m)
                        });
                    match result {
                        Ok(message) => {
                            route(
                                &sender.name,
                                std::slice::from_ref(&message),
                                &mut w.inbox,
                                tx,
                            );
                            // The lead sees every live handoff, even when not its direct recipient.
                            if message.to != self.config.leader.name {
                                w.inbox.entry(self.config.leader.name.clone()).or_default().push(json!({"from":sender.name,"body":format!("[to {}] {}",message.to,message.body)}));
                            }
                        }
                        Err(e) => emit(
                            tx,
                            "output",
                            &sender.name,
                            None,
                            format!("Rejected live message: {e}"),
                        ),
                    }
                }
                w.message_offsets.insert(key, offset + last + 1);
                changed = true;
            }
        }
        if changed {
            self.sync_inboxes(w)?;
            store.checkpoint(w)?;
        }
        Ok(())
    }
    pub(crate) async fn execute_assignments(
        &self,
        store: &mut Store,
        w: &mut Workflow,
        decision: &Decision,
        tx: &UnboundedSender<Event>,
    ) -> Result<()> {
        let round = w.round;
        let id_for = |index: usize| {
            format!(
                "{round:03}-{index:03}-{}",
                decision.assignments[index].agent
            )
        };
        let mut settled = BTreeSet::new();
        for index in 0..decision.assignments.len() {
            if w.tasks.iter().any(|t| {
                t.id == id_for(index)
                    && matches!(t.status.as_str(), "complete" | "failed" | "skipped")
            }) {
                settled.insert(index);
            }
        }
        let mut running = BTreeSet::new();
        let mut jobs = tokio::task::JoinSet::new();
        fs::write(self.directory.join("team/brief.md"), &decision.brief)?;
        self.sync_progress(w, decision)?;
        self.sync_inboxes(w)?;
        let mut dispatch_order: Vec<_> = (0..decision.assignments.len()).collect();
        // Reserve early capacity for QA/review even when the lead lists them last.
        // Very small pools retain the original order to avoid starving implementation.
        if self.config.max_parallel_agents >= 3 {
            dispatch_order.sort_by_key(|index| match decision.assignments[*index].agent.as_str() {
                "reviewer" => 0,
                "tester" => 1,
                _ => 2,
            });
        }
        loop {
            self.drain_messages(store, w, tx)?;
            for &index in &dispatch_order {
                if settled.contains(&index) || running.contains(&index) {
                    continue;
                }
                let assignment = &decision.assignments[index];
                let dependencies: Vec<_> = assignment
                    .depends_on
                    .iter()
                    .map(|name| {
                        decision
                            .assignments
                            .iter()
                            .position(|a| &a.agent == name)
                            .expect("validated dependency")
                    })
                    .collect();
                if dependencies.iter().any(|dependency| {
                    settled.contains(dependency)
                        && w.tasks
                            .iter()
                            .find(|t| t.id == id_for(*dependency))
                            .is_some_and(|t| t.status != "complete")
                }) {
                    self.record_assignment(
                        store,
                        w,
                        index,
                        assignment,
                        Err(anyhow::anyhow!(
                            "Dependency failed; task was not started: {}",
                            assignment.depends_on.join(", ")
                        )),
                        true,
                        tx,
                    )
                    .await?;
                    settled.insert(index);
                    continue;
                }
                if dependencies
                    .iter()
                    .any(|dependency| !settled.contains(dependency))
                {
                    continue;
                }
                if running.len() >= self.config.max_parallel_agents {
                    continue;
                }
                if running.iter().any(|other| {
                    protocol::scopes_conflict(assignment, &decision.assignments[*other])
                }) {
                    continue;
                }
                let config = self.current_config();
                let mut agent = config
                    .agents
                    .iter()
                    .find(|a| a.name == assignment.agent)
                    .context("Missing configured specialist")?
                    .clone();
                agent.team_inbox = Some(
                    self.directory
                        .join("team/inbox")
                        .join(format!("{}.json", agent.name)),
                );
                agent.team_outbox = Some(
                    self.directory
                        .join("team/outbox")
                        .join(format!("{}.jsonl", agent.name)),
                );
                agent.team_peers = std::iter::once(&self.config.leader)
                    .chain(&self.config.agents)
                    .map(|a| a.name.clone())
                    .collect();
                let id = id_for(index);
                let mut mail = w
                    .task(&id, &agent.name, &assignment.task)
                    .delivered_messages
                    .clone();
                mail.extend(w.inbox.remove(&agent.name).unwrap_or_default());
                let task = w.task(&id, &agent.name, &assignment.task);
                task.delivered_messages = mail.clone();
                task.status = "running".into();
                task.attempts += 1;
                w.status = "delegating".into();
                self.notify_observers(w, &agent.name, format!("Task started: {}. Inspectable scope: {}. Implementation may be partial; inspect actual files and send findings live.", assignment.task, serde_json::to_string(&assignment.files)?), tx);
                self.sync_progress(w, decision)?;
                self.sync_inboxes(w)?;
                store.checkpoint(w)?;
                emit(tx, "working", &agent.name, None, &assignment.task);
                let mut prompt = format!(
                    "{}\n\nYou are {}. Lead: {}. Peers: {}.\nGoal:\n{}\n\nShared architecture, stack, API and UI brief:\n{}\n\nTask assigned by lead:\n{}\n\nFile ownership: {}. Edit only these files/directory prefixes. An empty array means read-only; null means exclusive project ownership. Do not modify another agent's active files, shared manifests or lockfiles without a new coordinated assignment. Dependencies already completed: {}.\n\nInbox:\n{}\n\nRecent team reports:\n{}\n\nLIVE TEAM COMMUNICATION: Read {} before major edits and between tool calls for new peer handoffs. API agents can use send_message(to,body). CLI agents with write permission may append a single JSON object {{\"to\":\"peer-name\",\"body\":\"handoff\"}} followed by a newline to their own outbox {}. The coordinator routes it immediately, including to the lead. Do not edit peer inboxes, memory or consent files. Shared brief is also at {}. Structured final messages remain supported.\n\nWorkspace is {}. Execute assigned scope only. Do not spawn more agents, push, publish or deploy. Report actual changed files, checks and applied skills. Distinguish product defects from environment-only verification gaps (EPERM/listen/browser unavailable); put pending checks in verification_limits without claiming they passed. Return JSON matching:\n{}{}",
                    agent.instructions()?,
                    agent.name,
                    self.config.leader.name,
                    agent.team_peers.join(", "),
                    self.goal,
                    decision.brief,
                    assignment.task,
                    serde_json::to_string(&assignment.files)?,
                    assignment.depends_on.join(", "),
                    serde_json::to_string(&mail)?,
                    recent(&w.transcript),
                    agent.team_inbox.as_ref().unwrap().display(),
                    agent.team_outbox.as_ref().unwrap().display(),
                    self.directory.join("team/brief.md").display(),
                    if agent.full_access {
                        "FULL ACCESS (authorized); ownership still governs this task"
                    } else if self.write {
                        "writable specialists"
                    } else {
                        "READ ONLY; do not edit project files"
                    },
                    protocol::schema(false),
                    store.context(&agent.name, w)?
                );
                prompt.push_str(&format!("\n\nLIVE WORK STATUS: Read {} between bounded passes to see pending/running/complete/failed tasks and their current reports. Reviewer and tester start alongside implementation: inspect actual available snapshots, publish findings immediately, never wait indefinitely for queued tasks, and request a follow-up pass after later changes. Implementers must publish inspectable paths and contract updates to reviewer/tester after meaningful chunks. This file is coordinator-owned; do not edit it.\n", self.directory.join("team/progress.json").display()));
                let run = self.clone();
                let sender = tx.clone();
                jobs.spawn(async move {
                    let result = if run.demo {
                        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                        Ok(run.demo_report(&agent.name))
                    } else {
                        provider::invoke(
                            &agent,
                            false,
                            run.write,
                            &run.project,
                            &run.directory.join(id),
                            &prompt,
                            run.config.timeout_seconds,
                            &sender,
                        )
                        .await
                        .and_then(|raw| {
                            protocol::parse::<Report>(&raw).context("Invalid specialist response")
                        })
                    };
                    (index, result)
                });
                running.insert(index);
            }
            w.cursor = settled.len();
            self.sync_progress(w, decision)?;
            if settled.len() == decision.assignments.len() {
                break;
            }
            if running.is_empty() {
                bail!("No runnable tasks; check dependency graph and file ownership");
            }
            tokio::select! {
                result=jobs.join_next()=>{
                    let(index,result)=result.context("Missing running specialist")??;
                    running.remove(&index);
                    self.drain_messages(store,w,tx)?;
                    self.record_assignment(store,w,index,&decision.assignments[index],result,false,tx).await?;
                    settled.insert(index);w.cursor=settled.len();store.checkpoint(w)?;
                },
                _=tokio::time::sleep(std::time::Duration::from_millis(100))=>{},
            }
        }
        self.drain_messages(store, w, tx)?;
        self.sync_progress(w, decision)?;
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    async fn record_assignment(
        &self,
        store: &mut Store,
        w: &mut Workflow,
        index: usize,
        a: &protocol::Assignment,
        result: Result<Report>,
        skipped: bool,
        tx: &UnboundedSender<Event>,
    ) -> Result<()> {
        let id = format!("{:03}-{index:03}-{}", w.round, a.agent);
        let result = result.and_then(|report| {
            if report.summary.trim().is_empty() {
                bail!("Specialist summary cannot be empty");
            }
            report.memory.validate()?;
            protocol::validate_limits(&report.verification_limits)?;
            protocol::validate_messages(&report.messages, &self.config)?;
            Ok(report)
        });
        match result {
            Ok(report) => {
                w.failures.remove(&a.agent);
                let task = w.task(&id, &a.agent, &a.task);
                task.status = "complete".into();
                task.summary = Some(report.summary.clone());
                store.record(
                    w,
                    &a.agent,
                    &id,
                    &a.task,
                    &report.summary,
                    "complete",
                    &report.memory,
                );
                emit(
                    tx,
                    "report",
                    &a.agent,
                    Some(&self.config.leader.name),
                    &report.summary,
                );
                if !report.verification_limits.is_empty() {
                    emit(
                        tx,
                        "verification_limit",
                        &a.agent,
                        Some(&self.config.leader.name),
                        report.verification_limits.join("\n"),
                    );
                }
                route(&a.agent, &report.messages, &mut w.inbox, tx);
                w.transcript
                    .push(json!({"round":w.round,"from":a.agent,"report":report}));
                emit(
                    tx,
                    "ready",
                    &a.agent,
                    None,
                    "Report and memory saved for the lead",
                );
            }
            Err(error) => {
                let error = format!("{error:#}");
                w.failures.insert(a.agent.clone());
                let status = if skipped { "skipped" } else { "failed" };
                let task = w.task(&id, &a.agent, &a.task);
                task.status = status.into();
                task.summary = Some(error.clone());
                store.record(
                    w,
                    &a.agent,
                    &id,
                    &a.task,
                    &error,
                    status,
                    &Default::default(),
                );
                emit(
                    tx,
                    "error",
                    &a.agent,
                    Some(&self.config.leader.name),
                    &error,
                );
                w.transcript
                    .push(json!({"round":w.round,"from":a.agent,"error":error}));
            }
        }
        let task = w
            .tasks
            .iter()
            .find(|task| task.id == id)
            .expect("recorded assignment");
        self.notify_observers(w, &a.agent, format!("Task {}: {}. {}. Inspect current files; an earlier review/test receipt does not cover subsequent edits.", task.status, a.task, task.summary.as_deref().unwrap_or_default()), tx);
        self.sync_inboxes(w)?;
        store.checkpoint(w)?;
        persist(&self.directory, w).await?;
        emit_map(store, &a.agent, tx)?;
        Ok(())
    }
}
