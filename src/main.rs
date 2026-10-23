mod api;
mod api_tools;
mod catalog;
mod config;
mod control;
mod engine;
mod events;
mod memory;
mod permissions;
mod picker;
mod prompts;
mod protocol;
mod provider;
mod scheduler;
mod setup;
mod shell;
mod team;
mod ui;

use anyhow::{Context, Result, bail};
use clap::{CommandFactory, Parser, Subcommand};
use config::{Config, Provider};
use crossterm::event::{self, Event as KeyEvent};
use std::{
    io::{self, IsTerminal, Write},
    path::PathBuf,
};
use tokio::sync::mpsc;

#[derive(Parser)]
#[command(version, about = "Your agents. One lead. A complete engineering team.")]
struct Cli {
    #[arg(long, global = true, default_value = ".")]
    project: PathBuf,
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Generate a configurable team, contexts and role skills
    Init {
        #[arg(long, value_enum, default_value = "codex")]
        provider: Provider,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        base_url: Option<String>,
        /// Environment variable NAME, never the secret key
        #[arg(long)]
        api_key_env: Option<String>,
    },
    /// Run the lead and specialists against a project
    Run {
        goal: String,
        /// Simulate a team run without calling providers
        #[arg(long)]
        demo: bool,
        /// Permit edits; Full access applies to every agent after project consent
        #[arg(long)]
        write: bool,
        /// Explicitly keep all agents read-only, even when Full access is configured
        #[arg(long, conflicts_with = "write")]
        read_only: bool,
        /// Print events without the fullscreen dashboard
        #[arg(long)]
        no_tui: bool,
    },
    /// Continue the latest unfinished workflow from its durable checkpoint
    Resume {
        workflow: Option<String>,
        #[arg(long)]
        demo: bool,
        #[arg(long)]
        no_tui: bool,
    },
    /// Inspect shared project memory or one agent's memory map
    Memory {
        agent: Option<String>,
        #[arg(long)]
        demo: bool,
    },
    /// Upgrade unchanged legacy role prompts while preserving custom prompts
    UpgradePrompts,
    /// Validate configuration, installed CLIs and API environment variables
    Doctor,
    /// List connection presets (no network requests)
    Providers,
    /// List models from an API agent connection (no inference request)
    Models { agent: String },
    /// Inspect, authorize or revoke the project's permission mode
    Permissions {
        #[arg(value_enum)]
        mode: Option<config::Permissions>,
        /// Explicitly authorize Full access without an interactive prompt
        #[arg(long)]
        yes: bool,
    },
    /// Configure each agent prompt, skills, provider and model before running
    Configure,
    /// Print a saved events.jsonl session (never executes agents)
    Replay { path: PathBuf },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let project = cli
        .project
        .canonicalize()
        .context("Project directory does not exist")?;
    let config_path = cli
        .config
        .map(|p| if p.is_absolute() { p } else { project.join(p) })
        .unwrap_or_else(|| project.join(".ai-lead/team.toml"));
    let command = match cli.command {
        Some(command) => command,
        None => {
            if !io::stdout().is_terminal() || !io::stdin().is_terminal() {
                Cli::command().print_help()?;
                println!();
                return Ok(());
            }
            let mut screen = ui::Screen::enter()?;
            let new_project = !config_path.exists();
            if new_project {
                if config_path != project.join(".ai-lead/team.toml") {
                    bail!(
                        "Custom configuration does not exist: {}",
                        config_path.display()
                    );
                }
                if !screen.onboard(&project)? {
                    return Ok(());
                }
                let _lock = engine::lock(&project)?;
                config::init(&project, Provider::Codex)?;
            }
            {
                let _lock = engine::lock(&project)?;
                config::upgrade_defaults(&config_path)?;
            }
            let mut config = Config::load_for_edit(&config_path)?;
            if config.setup_pending {
                if !new_project && !screen.onboard(&project)? {
                    return Ok(());
                }
                let _lock = engine::lock(&project)?;
                screen.configure(&mut config, &config_path)?;
                if config.setup_pending {
                    return Ok(());
                }
            }
            let launch = screen.launch(&mut config, &project, &config_path)?;
            drop(screen);
            let Some(launch) = launch else {
                return Ok(());
            };
            if launch.resume {
                Commands::Resume {
                    workflow: None,
                    demo: launch.mode == 0,
                    no_tui: false,
                }
            } else {
                Commands::Run {
                    goal: launch.goal,
                    demo: launch.mode == 0,
                    write: launch.mode == 2,
                    read_only: launch.mode == 1,
                    no_tui: false,
                }
            }
        }
    };
    match command {
        Commands::Init {
            provider,
            model,
            base_url,
            api_key_env,
        } => {
            let api = if base_url.is_some() || api_key_env.is_some() {
                Some(config::ApiConfig {
                    base_url,
                    api_key_env,
                    ..Default::default()
                })
            } else {
                None
            };
            let path = config::init_with_connection(&project, provider, model, api)?;
            println!(
                "Created {}\n\nTeam: lead + designer, frontend, backend, architect, tester, reviewer\nEdit models, reasoning, contexts and skills in team.toml.\n\nTry: ai-lead run --demo \"Build a task manager\"",
                path.display()
            );
            Ok(())
        }
        Commands::Run {
            goal,
            demo,
            write,
            read_only,
            no_tui,
        } => {
            let mode = Config::load(&config_path)?.permissions;
            let write = !read_only && (write || (!demo && mode == config::Permissions::FullAccess));
            start_run(&project, &config_path, goal, demo, write, no_tui, None).await
        }
        Commands::Resume {
            workflow,
            demo,
            no_tui,
        } => {
            let store = memory::Store::load(&project, demo)?;
            let saved = store
                .pending(workflow.as_deref())
                .context("No unfinished workflow to resume in this memory store")?;
            start_run(
                &project,
                &config_path,
                saved.goal,
                demo,
                saved.write,
                no_tui,
                Some(saved.id),
            )
            .await
        }
        Commands::Memory { agent, demo } => {
            let store = memory::Store::load(&project, demo)?;
            let value = if let Some(name) = agent {
                store.agent_map(&name)
            } else {
                serde_json::json!({"root":store.root,"shared":store.state.shared,"agents":store.state.agents.keys().map(|name|(name.clone(),store.agent_map(name))).collect::<std::collections::BTreeMap<_,_>>(),"workflows":store.state.workflows.values().map(|w|serde_json::json!({"id":w.id,"goal":w.goal,"status":w.status,"round":w.round})).collect::<Vec<_>>()})
            };
            println!("{}", serde_json::to_string_pretty(&value)?);
            Ok(())
        }
        Commands::UpgradePrompts => {
            let _lock = engine::lock(&project)?;
            let count = config::upgrade_defaults(&config_path)?;
            println!("Upgraded {count} unchanged default prompts. Custom prompts were preserved.");
            Ok(())
        }
        Commands::Permissions { mode, yes } => {
            let mut config = Config::load_for_edit(&config_path)?;
            match mode {
                None => println!(
                    "Mode: {:?}; project full-access consent: {}",
                    config.permissions,
                    permissions::granted(&project)?
                ),
                Some(config::Permissions::Restricted) => {
                    let _lock = engine::lock(&project)?;
                    permissions::revoke(&project)?;
                    config.permissions = config::Permissions::Restricted;
                    config.save(&config_path)?;
                    println!("Restricted mode saved; Full access consent revoked.");
                }
                Some(config::Permissions::FullAccess) => {
                    let _lock = engine::lock(&project)?;
                    if !permissions::confirm_cli(&project, yes)? {
                        println!("Full access not authorized.");
                        return Ok(());
                    }
                    config.permissions = config::Permissions::FullAccess;
                    config.save(&config_path)?;
                    println!(
                        "Full access authorized for every agent in {}",
                        project.display()
                    );
                }
            }
            Ok(())
        }
        Commands::Configure => {
            let _lock = engine::lock(&project)?;
            config::upgrade_defaults(&config_path)?;
            let mut config = Config::load_for_edit(&config_path)?;
            let mut screen = ui::Screen::enter()?;
            screen.configure(&mut config, &config_path)
        }
        Commands::Providers => {
            for p in Provider::ALL {
                let (url, key) = p.defaults();
                println!(
                    "{} — {}: {}  {}",
                    serde_json::to_string(&p)?,
                    p.label(),
                    url,
                    key
                );
            }
            Ok(())
        }
        Commands::Models { agent } => {
            let config = Config::load_for_edit(&config_path)?;
            let a = std::iter::once(&config.leader)
                .chain(&config.agents)
                .find(|a| a.name == agent)
                .context("Unknown agent")?;
            for model in api::models(a).await? {
                println!("{model}");
            }
            Ok(())
        }
        Commands::Doctor => {
            let config = Config::load(&config_path)?;
            println!(
                "Configuration OK: {} specialists; {} rounds; {}s timeout",
                config.agents.len(),
                config.max_rounds,
                config.timeout_seconds
            );
            for provider in [Provider::Codex, Provider::Claude] {
                if !std::iter::once(&config.leader)
                    .chain(&config.agents)
                    .any(|a| a.provider == provider)
                {
                    continue;
                }
                let executable = match provider {
                    Provider::Codex => "codex",
                    Provider::Claude => "claude",
                    _ => unreachable!(),
                };
                let out = tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    tokio::process::Command::new(executable)
                        .arg("--version")
                        .kill_on_drop(true)
                        .output(),
                )
                .await;
                match out {
                    Ok(Ok(out)) if out.status.success() => println!(
                        "{executable}: {}",
                        String::from_utf8_lossy(&out.stdout).trim()
                    ),
                    _ => bail!("{executable}: unavailable; install and sign in"),
                }
            }
            for a in std::iter::once(&config.leader)
                .chain(&config.agents)
                .filter(|a| a.provider.is_api())
            {
                api::credentials(a)?;
                println!(
                    "{}: {} / {} — environment OK",
                    a.name,
                    a.provider.label(),
                    a.model.as_deref().unwrap_or("")
                );
            }
            println!(
                "Model availability and authentication are checked by the provider on a live run."
            );
            Ok(())
        }
        Commands::Replay { path } => {
            for (index, line) in std::fs::read_to_string(path)?.lines().enumerate() {
                let e: events::Event = serde_json::from_str(line)
                    .with_context(|| format!("Invalid event on line {}", index + 1))?;
                print_event(&e);
            }
            Ok(())
        }
    }
}

async fn start_run(
    project: &std::path::Path,
    config_path: &std::path::Path,
    goal: String,
    demo: bool,
    write: bool,
    no_tui: bool,
    resume: Option<String>,
) -> Result<()> {
    if goal.trim().is_empty() {
        bail!("Goal cannot be empty");
    }
    let _lock = engine::lock(project)?;
    config::upgrade_defaults(config_path)?;
    let mut config = Config::load(config_path)?;
    let full_access = !demo && write && config.permissions == config::Permissions::FullAccess;
    if full_access && !permissions::granted(project)? {
        let allowed = if !no_tui && io::stdin().is_terminal() && io::stdout().is_terminal() {
            let mut screen = ui::Screen::enter()?;
            let allowed = screen.authorize_full_access(project)?;
            if allowed {
                permissions::grant(project)?;
            }
            allowed
        } else {
            permissions::confirm_cli(project, false)?
        };
        if !allowed {
            println!("Full access not authorized. No agents were started.");
            return Ok(());
        }
    }
    for agent in std::iter::once(&mut config.leader).chain(&mut config.agents) {
        agent.full_access = full_access;
    }
    let directory = project
        .join(".ai-lead/runs")
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&directory)?;
    std::fs::write(
        directory.join("session.json"),
        serde_json::to_vec_pretty(
            &serde_json::json!({"goal":goal,"project":project,"demo":demo,"write":write,"full_access":full_access,"resume":resume,"config":config}),
        )?,
    )?;
    let run = engine::Run {
        live_config: std::sync::Arc::new(std::sync::RwLock::new(config.clone())),
        config,
        project: project.into(),
        directory,
        goal,
        demo,
        write,
        resume,
    };
    run_session(
        run,
        config_path,
        !no_tui && io::stdout().is_terminal() && io::stdin().is_terminal(),
    )
    .await
}

async fn run_session(mut run: engine::Run, config_path: &std::path::Path, tui: bool) -> Result<()> {
    let mut log =
        std::io::BufWriter::new(std::fs::File::create(run.directory.join("events.jsonl"))?);
    let mut screen = if tui {
        Some(ui::Screen::enter()?)
    } else {
        None
    };
    let mut view = ui::View::with_log_capacity(run.config.log_buffer_events);
    view.motion = std::env::var("AI_LEAD_REDUCED_MOTION").as_deref() != Ok("1");
    let (tx, mut rx) = mpsc::unbounded_channel();
    let runner = run.clone();
    let producer = tx.clone();
    let mut task = Some(tokio::spawn(async move { runner.execute(&producer).await }));
    let mut result: Option<Result<String>> = None;
    let mut dirty = true;
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(100));
    'session: loop {
        tokio::select! {
            Some(e) = rx.recv() => {
                serde_json::to_writer(&mut log,&e)?;
                writeln!(&mut log)?;
                if !tui { print_event(&e); }
                view.apply(e);
                dirty = true;
            },
            _ = tick.tick() => {
                log.flush()?;
                if task.as_ref().is_some_and(|t| t.is_finished()) {
                    let outcome = task.take().context("Missing run task")?.await.unwrap_or_else(|e| Err(e.into()));
                    if let Err(error) = &outcome { events::emit(&tx,"session_error","system",None,format!("{error:#}")); }
                    result = Some(outcome);
                }
                if !tui && result.is_some() && rx.is_empty() { break; }
                if let Some(screen) = &mut screen {
                    for _ in 0..128 {
                    let input = if event::poll(std::time::Duration::ZERO)? { dirty = true; Some(event::read()?) } else { None };
                    if input.is_none() { break; }
                    match input {
                        Some(KeyEvent::Key(key)) if view.handle_key(key, run.config.agents.len()+1) => break 'session,
                        Some(KeyEvent::Paste(text)) => view.paste(&text),
                        _ => {},
                    }
                    if let Some(action) = view.action.take() {
                        match action {
                            ui::Action::Send(text) => match control::send(&run.directory, &run.config.leader.name, &text) {
                                Ok(()) => {
                                    view.draft = Default::default();
                                    view.composing = false;
                                    view.notice = "Saved for lead · next planning turn".into();
                                    events::emit(&tx, "user_queued", "user", Some(&run.config.leader.name), format!("Saved for the lead: {text}"));
                                },
                                Err(error) => view.notice = format!("Not sent; draft kept: {error:#}"),
                            },
                            ui::Action::Configure => {
                                let before = serde_json::to_value(&run.config)?;
                                match screen.configure_agent(&mut run.config, config_path, view.selected, &run.live_config) {
                                    Ok(()) => {
                                        if serde_json::to_value(&run.config)? != before {
                                            *run.live_config.write().unwrap_or_else(|e| e.into_inner()) = run.config.clone();
                                            view.notice = "Settings saved · apply on next agent invocation".into();
                                            events::emit(&tx, "settings", "system", Some(&run.config.leader.name), "Team settings refreshed. New invocations use the saved models/providers; current calls continue unchanged.");
                                        } else { view.notice = "Settings closed · no changes".into(); }
                                    },
                                    Err(error) => view.notice = format!("Settings error: {error:#}"),
                                }
                            },
                        }
                        dirty = true;
                    }
                    }
                    if task.is_none() && result.is_some() {
                        let store = memory::Store::load(&run.project, run.demo)?;
                        if let Some(workflow) = store.state.active.as_ref().and_then(|id| store.state.workflows.get(id))
                            && workflow.session == run.directory && run.has_pending_user_messages(workflow)
                        {
                            run.resume = Some(workflow.id.clone());
                            let runner = run.clone();
                            let producer = tx.clone();
                            task = Some(tokio::spawn(async move { runner.execute(&producer).await }));
                            result = None;
                            view.status = "RUNNING".into();
                            view.notice = "Follow-up saved · lead is planning".into();
                        }
                    }
                    let clock_dirty = result.is_none() && view.advance();
                    if dirty || clock_dirty || (result.is_none() && view.motion) { screen.draw(&view,&run.config,&run.goal,run.demo)?; dirty = false; }
                }
            },
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    if let Some(task) = task {
        task.abort();
        let _ = task.await;
        let mut store = memory::Store::load(&run.project, run.demo)?;
        if let Some(mut workflow) = store.pending(None)
            && workflow.session == run.directory
        {
            workflow.status = "interrupted".into();
            workflow.last_error=Some("Stopped by user; inspect partial effects of the current running task before continuing.".into());
            store.checkpoint(&workflow)?;
        }
        events::emit(
            &tx,
            "cancelled",
            "system",
            None,
            "Cancelled by user. Partial file changes and reports remain in the workspace.",
        );
    }
    while let Ok(e) = rx.try_recv() {
        serde_json::to_writer(&mut log, &e)?;
        writeln!(&mut log)?;
        if !tui {
            print_event(&e);
        }
        view.apply(e);
    }
    log.flush()?;
    std::fs::write(
        run.directory.join("status.json"),
        serde_json::to_vec_pretty(&serde_json::json!({"status":view.status,"round":view.round}))?,
    )?;
    drop(screen);
    println!("Session: {}", run.directory.display());
    match result {
        Some(Ok(summary)) => {
            println!("{summary}");
            Ok(())
        }
        Some(Err(error)) => Err(error),
        None => {
            println!("Stopped. Changes and session logs were preserved.");
            Ok(())
        }
    }
}

fn print_event(e: &events::Event) {
    let target =
        e.to.as_ref()
            .map(|to| format!(" → {to}"))
            .unwrap_or_default();
    let clean: String = e
        .text
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect();
    println!("[{}] {}{}: {}", e.kind, e.agent, target, clean);
}
