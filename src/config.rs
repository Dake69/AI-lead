use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

pub const ROLES: [&str; 6] = [
    "designer",
    "frontend",
    "backend",
    "architect",
    "tester",
    "reviewer",
];

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Codex,
    Claude,
    Openai,
    Anthropic,
    Gemini,
    Openrouter,
    Deepseek,
    Groq,
    Xai,
    Mistral,
    Compatible,
}

impl Provider {
    pub const ALL: [Self; 11] = [
        Self::Codex,
        Self::Claude,
        Self::Openai,
        Self::Anthropic,
        Self::Gemini,
        Self::Openrouter,
        Self::Deepseek,
        Self::Groq,
        Self::Xai,
        Self::Mistral,
        Self::Compatible,
    ];
    pub fn label(&self) -> &'static str {
        match self {
            Self::Codex => "Codex CLI",
            Self::Claude => "Claude Code",
            Self::Openai => "OpenAI API",
            Self::Anthropic => "Anthropic API",
            Self::Gemini => "Gemini",
            Self::Openrouter => "OpenRouter",
            Self::Deepseek => "DeepSeek",
            Self::Groq => "Groq",
            Self::Xai => "xAI",
            Self::Mistral => "Mistral",
            Self::Compatible => "Custom API",
        }
    }
    pub fn is_api(&self) -> bool {
        !matches!(self, Self::Codex | Self::Claude)
    }
    pub fn defaults(&self) -> (&'static str, &'static str) {
        match self {
            Self::Openai => ("https://api.openai.com/v1", "OPENAI_API_KEY"),
            Self::Anthropic => ("https://api.anthropic.com/v1", "ANTHROPIC_API_KEY"),
            Self::Gemini => (
                "https://generativelanguage.googleapis.com/v1beta/openai",
                "GEMINI_API_KEY",
            ),
            Self::Openrouter => ("https://openrouter.ai/api/v1", "OPENROUTER_API_KEY"),
            Self::Deepseek => ("https://api.deepseek.com", "DEEPSEEK_API_KEY"),
            Self::Groq => ("https://api.groq.com/openai/v1", "GROQ_API_KEY"),
            Self::Xai => ("https://api.x.ai/v1", "XAI_API_KEY"),
            Self::Mistral => ("https://api.mistral.ai/v1", "MISTRAL_API_KEY"),
            _ => ("", ""),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ApiConfig {
    pub base_url: Option<String>,
    pub api_key_env: Option<String>,
    pub max_output_tokens: Option<u32>,
    pub max_tool_rounds: Option<usize>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Permissions {
    #[default]
    FullAccess,
    Restricted,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Agent {
    #[serde(skip)]
    pub team_inbox: Option<PathBuf>,
    #[serde(skip)]
    pub team_outbox: Option<PathBuf>,
    #[serde(skip)]
    pub team_peers: Vec<String>,
    /// Runtime capability, set only after project consent; never loaded from TOML.
    #[serde(skip)]
    pub full_access: bool,
    pub name: String,
    pub provider: Provider,
    pub model: Option<String>,
    pub reasoning: Option<String>,
    pub context: PathBuf,
    #[serde(default)]
    pub skills: Vec<PathBuf>,
    #[serde(default)]
    pub extra_args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<ApiConfig>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_parallel_agents")]
    pub max_parallel_agents: usize,
    #[serde(default)]
    pub permissions: Permissions,
    /// First interactive launch must finish project-local setup.
    #[serde(default)]
    pub setup_pending: bool,
    #[serde(default = "default_log_buffer")]
    pub log_buffer_events: usize,
    pub max_rounds: usize,
    pub timeout_seconds: u64,
    pub leader: Agent,
    pub agents: Vec<Agent>,
}

pub fn default_log_buffer() -> usize {
    2000
}
pub fn default_parallel_agents() -> usize {
    6
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let config = Self::load_for_edit(path)?;
        config.validate()?;
        Ok(config)
    }

    /// Missing skill files must not prevent opening the editor to repair their paths.
    pub fn load_for_edit(path: &Path) -> Result<Self> {
        let mut config: Self = toml::from_str(&fs::read_to_string(path).with_context(|| {
            format!("Cannot read {}. Run `ai-lead init` first.", path.display())
        })?)?;
        let root = path.parent().unwrap_or(Path::new("."));
        for agent in std::iter::once(&mut config.leader).chain(config.agents.iter_mut()) {
            agent.context = root.join(&agent.context);
            agent.skills = agent
                .skills
                .iter()
                .map(|p| resolve_skill(root, p))
                .collect();
        }
        Ok(config)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let root = path.parent().unwrap_or(Path::new("."));
        let mut portable = self.clone();
        for agent in std::iter::once(&mut portable.leader).chain(&mut portable.agents) {
            if let Ok(relative) = agent.context.strip_prefix(root) {
                agent.context = relative.to_owned();
            }
            for skill in &mut agent.skills {
                if let Ok(relative) = skill.strip_prefix(root) {
                    *skill = relative.to_owned();
                }
            }
        }
        let temporary = root.join(format!(".team-{}.tmp", uuid::Uuid::new_v4()));
        let text = format!(
            "# AI Lead team configuration. Paths are relative to this file.\n{}",
            toml::to_string_pretty(&portable)?
        );
        fs::write(&temporary, text)?;
        if let Err(error) = fs::rename(&temporary, path) {
            let _ = fs::remove_file(&temporary);
            return Err(error.into());
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        if !(1..=16).contains(&self.max_parallel_agents) {
            bail!("max_parallel_agents must be 1..16");
        }
        if !(100..=20_000).contains(&self.log_buffer_events) {
            bail!("log_buffer_events must be 100..20000 per agent");
        }
        if !(1..=100).contains(&self.max_rounds) {
            bail!("max_rounds must be 1..100");
        }
        if self.timeout_seconds == 0 {
            bail!("timeout_seconds must be positive");
        }
        if self.agents.is_empty() {
            bail!("At least one specialist is required");
        }
        let mut names = HashSet::new();
        for a in std::iter::once(&self.leader).chain(self.agents.iter()) {
            if a.name.is_empty()
                || !a
                    .name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            {
                bail!("Agent names must contain only ASCII letters, digits, - and _");
            }
            if !names.insert(&a.name) {
                bail!("Duplicate agent name: {}", a.name);
            }
            if a.model.as_ref().is_some_and(|m| m.trim().is_empty()) {
                bail!("Empty model for {}", a.name);
            }
            if let Some(reasoning) = &a.reasoning {
                if !["minimal", "low", "medium", "high", "xhigh"].contains(&reasoning.as_str()) {
                    bail!("Unsupported reasoning value for {}", a.name);
                }
                if !matches!(a.provider, Provider::Codex | Provider::Openai) {
                    bail!("reasoning is supported for Codex CLI and OpenAI Responses only");
                }
            }
            if a.provider.is_api() {
                if a.model.is_none() {
                    bail!("{}: API model is required", a.name);
                }
                if !a.extra_args.is_empty() {
                    bail!("{}: extra_args are only supported by CLI providers", a.name);
                }
                crate::api::settings(a)?;
            }
            a.instructions()?;
        }
        Ok(())
    }
}

impl Agent {
    pub fn role_prompt(&self) -> Result<String> {
        let raw = fs::read_to_string(&self.context)
            .with_context(|| format!("Missing context: {}", self.context.display()))?;
        Ok(crate::prompts::with_skills(&raw, &self.skills))
    }
    pub fn instructions(&self) -> Result<String> {
        let mut text = self.role_prompt()?;
        for skill in &self.skills {
            text.push_str(&format!(
                "\n\n<skill name={:?} path={:?}>\n{}\n</skill>",
                crate::prompts::skill_name(skill),
                skill,
                fs::read_to_string(skill)
                    .with_context(|| format!("Missing skill: {}", skill.display()))?
            ));
        }
        if self.full_access {
            text.push_str("\n\n<RUNTIME_PERMISSIONS>FULL ACCESS explicitly authorized by the user for this project and all team members, including the lead. This overrides legacy sandbox capability descriptions, never role responsibilities or assigned ownership. You may use available shell, network and file tools with the OS account's permissions, including outside the project when the task requires it. Stay within the user's task and your role; access is capability, not permission for unrelated actions. Do not modify AI Lead runtime state or consent files.</RUNTIME_PERMISSIONS>");
        }
        text.push_str(&format!(
            "\n\n<ROLE_BOUNDARY>{}</ROLE_BOUNDARY>",
            crate::prompts::role_boundary(&self.name)
        ));
        Ok(text)
    }
}

pub fn init(project: &Path, provider: Provider) -> Result<PathBuf> {
    init_with_connection(project, provider, None, None)
}

pub fn init_with_connection(
    project: &Path,
    provider: Provider,
    model: Option<String>,
    api: Option<ApiConfig>,
) -> Result<PathBuf> {
    let root = project.join(".ai-lead");
    let path = root.join("team.toml");
    if path.exists() {
        bail!(
            "{} already exists; existing configuration was preserved",
            path.display()
        );
    }
    let leader = Agent {
        team_inbox: None,
        team_outbox: None,
        team_peers: vec![],
        full_access: false,
        name: "lead".into(),
        provider,
        model,
        reasoning: None,
        context: "contexts/lead.md".into(),
        skills: vec!["skills/lead.md".into()],
        extra_args: vec![],
        api,
    };
    if leader.provider.is_api() {
        if leader.model.as_ref().is_none_or(|m| m.trim().is_empty()) {
            bail!("API provider requires --model (use a tool-capable model ID from your account)");
        }
        crate::api::settings(&leader)?;
    }
    fs::create_dir_all(root.join("contexts"))?;
    fs::create_dir_all(root.join("skills"))?;
    if !root.join(".gitignore").exists() {
        fs::write(
            root.join(".gitignore"),
            "/runs/\n/run.lock\n/memory/\n/demo-memory/\n/permissions.json\n",
        )?;
    }
    let agents = ROLES
        .iter()
        .map(|role| Agent {
            name: (*role).into(),
            context: format!("contexts/{role}.md").into(),
            skills: vec![format!("skills/{role}.md").into()],
            ..leader.clone()
        })
        .collect();
    let config = Config {
        max_parallel_agents: default_parallel_agents(),
        permissions: Permissions::FullAccess,
        setup_pending: true,
        log_buffer_events: default_log_buffer(),
        max_rounds: 12,
        timeout_seconds: 900,
        leader,
        agents,
    };
    for a in std::iter::once(&config.leader).chain(config.agents.iter()) {
        let (_, skill) = legacy_template(&a.name);
        let context = crate::prompts::with_skills(&crate::prompts::base(&a.name), &a.skills);
        // Never replace a user's customized role files during a repeated/partial init.
        for (relative, content) in [(&a.context, context.as_str()), (&a.skills[0], skill)] {
            let target = root.join(relative);
            if !target.exists() {
                fs::write(target, content)?;
            }
        }
    }
    fs::write(
        &path,
        format!(
            "# AI Lead team configuration. Paths are relative to this file.\n# Set model to an ID available in your provider account.\n# Codex supports reasoning = \"medium\" / \"xhigh\" (model dependent).\n# Each specialist can use a different provider and model.\n{}",
            toml::to_string_pretty(&config)?
        ),
    )?;
    Ok(path)
}

pub fn resolve_skill(root: &Path, path: &Path) -> PathBuf {
    let raw = path.to_string_lossy();
    let path = if let Some(rest) = raw.strip_prefix("~/") {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join(rest))
            .unwrap_or_else(|| root.join(path))
    } else {
        root.join(path)
    };
    if path.is_dir() {
        path.join("SKILL.md")
    } else {
        path
    }
}

pub fn upgrade_defaults(path: &Path) -> Result<usize> {
    let config = Config::load_for_edit(path)?;
    let root = path.parent().unwrap_or(Path::new("."));
    let mut upgraded = 0;
    for agent in std::iter::once(&config.leader).chain(&config.agents) {
        for skill in &agent.skills {
            if *skill == root.join("skills").join(format!("{}.md", agent.name))
                && fs::read_to_string(skill)
                    .is_ok_and(|raw| crate::prompts::is_previous_skill(&agent.name, &raw))
            {
                fs::write(skill, legacy_template(&agent.name).1)?;
            }
        }
        if !agent.context.starts_with(root.join("contexts")) {
            continue;
        }
        let raw = fs::read_to_string(&agent.context)?;
        let base = crate::prompts::without_skills(&raw);
        if base == legacy_template(&agent.name).0
            || crate::prompts::is_previous_base(&agent.name, &base)
        {
            fs::write(
                &agent.context,
                crate::prompts::with_skills(&crate::prompts::base(&agent.name), &agent.skills),
            )?;
            upgraded += 1;
        }
    }
    Ok(upgraded)
}

fn legacy_template(role: &str) -> (&'static str, &'static str) {
    match role {
        "lead" => (
            "You are the technical lead and sole decision maker. Inspect the project, decompose the goal, assign concrete tasks only to listed specialists, and define acceptance criteria. Delegate implementation; do not edit project files yourself. Read every report and message. Route dependencies to relevant peers. Ask the tester and reviewer to verify changed code before finishing. Failures require explicit disposition. Only you may declare the task complete. Never claim unverified checks passed.",
            "# Orchestration\nDelegate and coordinate only; write no product code, tests or project configuration. Architect owns architecture decisions. Dispatch backend/frontend/designer with precise separate ownership and tester/reviewer alongside them. Tester alone writes tests and runs tests/builds/browser checks; reviewer performs live read-only logic review and sends findings immediately. Request current final checks/review after repairs, then personally launch the project using tester-provided startup instructions. Preserve honest verification limits and continuation memory.",
        ),
        "frontend" => (
            "You are the frontend engineer. Implement accessible, responsive interfaces using project conventions. Coordinate contracts with backend and visuals with designer. Report changed files and checks to lead.",
            "# Frontend applications\nImplement web/mobile/desktop/terminal application UI and client behavior only. Reuse designer-owned visual primitives and backend contracts. Do not write tests, fixtures or test configuration, or run tester-owned builds/browser checks. Send inspectable code paths to reviewer/tester during implementation. Fix your production code from their findings.",
        ),
        "backend" => (
            "You are the backend engineer. Own application logic, APIs and persistence. Coordinate interfaces with frontend and deployment requirements with devops. Report all results to lead.",
            "# Backend\nWrite server/domain/API/persistence code only. Validate boundaries, specify contracts and handle errors explicitly. Never write tests, fixtures, test configuration or frontend code. Tester writes tests alongside implementation and runs verification/builds. Share inspectable paths and contract changes with reviewer/tester immediately; fix only backend production defects.",
        ),
        "architect" | "devops" => (
            "You are the DevOps engineer. Own builds, CI, runtime configuration and operational documentation. Coordinate runtime requirements with backend. Do not deploy, publish or push without explicit user authorization.",
            "# Architecture\nChoose stack and modular monolith or justified services when unspecified. Own readable structure, explicit contracts and assigned structural/configuration files. Apply SOLID and suitable patterns idiomatically. Do not implement features or tests, or take over tester verification. Publish decisions promptly for parallel implementers and reviewer.",
        ),
        "tester" => (
            "You are the test engineer. Independently verify acceptance criteria and regressions. Report reproducible failures to owners and lead. Do not silently weaken tests.",
            "# Verification ownership\nAlone write/edit all tests, fixtures, harnesses and test configuration. Start contract-based tests while backend/frontend implement. Run ordinary tests, format/lint/type checks, builds and real browser/native/visual checks where available. Report reproducible failures live to owner/lead without patching product code. Rerun after fixes and final changes; record evidence and environment-only limits. Supply lead the verified startup command, working directory and URL.",
        ),
        "reviewer" => (
            "You are the code reviewer. Inspect actual changes for correctness, security, regressions and maintainability. Send actionable findings to the responsible specialist and lead. Do not approve without evidence.",
            "# Continuous code review\nBegin read-only logic review across backend/frontend/design/architecture/tests while code is written. Read actual files and progress/inbox between bounded passes. Send supported findings live to owner/lead; do not patch code/tests or run tester verification. Recheck fixes and final changes; never treat an early pass as approval of unseen code.",
        ),
        _ => (
            "You are the product designer. Define coherent visual direction and interaction details grounded in the current product. Coordinate a concrete specification with frontend and report to lead.",
            "# Design\nOwn visual tokens/styles and agreed visual primitives alongside frontend. Define named blocks, sections, cards and highly responsive accessible states. For new identities use two or three font families including a brand face and two or three main responsive size tokens; use compatible external libraries intentionally. Do not write tests or application/backend behavior. Give tester visual acceptance criteria; tester runs builds/browser checks. Share inspectable visual code with reviewer immediately.",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn upgrades_only_unchanged_legacy_prompts() {
        let dir = tempfile::tempdir().unwrap();
        let path = init(dir.path(), Provider::Codex).unwrap();
        let config = Config::load(&path).unwrap();
        fs::write(&config.leader.context, legacy_template("lead").0).unwrap();
        fs::write(&config.agents[0].context, "Strict user-authored prompt").unwrap();
        assert_eq!(upgrade_defaults(&path).unwrap(), 1);
        assert!(fs::read_to_string(&config.leader.context).unwrap().len() > 7000);
        assert_eq!(
            fs::read_to_string(&config.agents[0].context).unwrap(),
            "Strict user-authored prompt"
        );
        assert_eq!(upgrade_defaults(&path).unwrap(), 0);
    }

    #[test]
    fn parallel_templates_and_skills_upgrade_but_custom_text_and_worker_limit_survive() {
        let dir = tempfile::tempdir().unwrap();
        let path = init(dir.path(), Provider::Codex).unwrap();
        let mut config = Config::load(&path).unwrap();
        config.max_parallel_agents = 4;
        config.save(&path).unwrap();
        let old = format!(
            "{}\n\n{}",
            include_str!("prompts/previous_parallel/common.md"),
            include_str!("prompts/previous_parallel/lead.md")
        );
        fs::write(
            &config.leader.context,
            crate::prompts::with_skills(&old, &config.leader.skills),
        )
        .unwrap();
        fs::write(
            &config.leader.skills[0],
            include_str!("prompts/previous_parallel/lead-skill.md"),
        )
        .unwrap();
        let tester = config.agents.iter().find(|a| a.name == "tester").unwrap();
        fs::write(
            &tester.skills[0],
            include_str!("prompts/previous_parallel/tester-skill.md"),
        )
        .unwrap();
        let frontend = config.agents.iter().find(|a| a.name == "frontend").unwrap();
        fs::write(&frontend.context, "Exact custom frontend context\n ").unwrap();
        fs::write(&frontend.skills[0], "Exact custom skill\n ").unwrap();
        assert_eq!(upgrade_defaults(&path).unwrap(), 1);
        assert!(
            fs::read_to_string(&config.leader.context)
                .unwrap()
                .contains("Your job is ONLY")
        );
        assert!(
            fs::read_to_string(&config.leader.skills[0])
                .unwrap()
                .contains("write no product code")
        );
        assert!(
            fs::read_to_string(&tester.skills[0])
                .unwrap()
                .contains("Alone write/edit all tests")
        );
        assert_eq!(
            fs::read_to_string(&frontend.context).unwrap(),
            "Exact custom frontend context\n "
        );
        assert_eq!(
            fs::read_to_string(&frontend.skills[0]).unwrap(),
            "Exact custom skill\n "
        );
        assert_eq!(Config::load(&path).unwrap().max_parallel_agents, 4);
        assert_eq!(upgrade_defaults(&path).unwrap(), 0);
        let mut frontend = frontend.clone();
        frontend.full_access = true;
        let instructions = frontend.instructions().unwrap();
        assert!(instructions.contains("FULL ACCESS"));
        assert!(instructions.ends_with("</ROLE_BOUNDARY>"));
        assert!(instructions.contains("Do not write/edit tests"));
    }

    #[test]
    fn previous_expanded_prompts_upgrade_without_overwriting_custom_prompts_or_skills() {
        let dir = tempfile::tempdir().unwrap();
        let path = init(dir.path(), Provider::Codex).unwrap();
        let config = Config::load(&path).unwrap();
        fs::write(
            &config.leader.context,
            crate::prompts::with_skills(
                &crate::prompts::previous_base("lead"),
                &config.leader.skills,
            ),
        )
        .unwrap();
        let architect = config
            .agents
            .iter()
            .find(|a| a.name == "architect")
            .unwrap();
        fs::write(&architect.context, crate::prompts::previous_base("devops")).unwrap();
        fs::write(
            &architect.skills[0],
            crate::prompts::previous_skill("devops").unwrap(),
        )
        .unwrap();
        let designer = config.agents.iter().find(|a| a.name == "designer").unwrap();
        fs::write(&designer.context, "Exact user custom designer instructions").unwrap();
        fs::write(&designer.skills[0], "Exact custom design skill").unwrap();
        assert_eq!(upgrade_defaults(&path).unwrap(), 2);
        assert!(
            fs::read_to_string(&config.leader.context)
                .unwrap()
                .contains("parallel delivery coordinator")
        );
        assert!(
            fs::read_to_string(&architect.context)
                .unwrap()
                .contains("# Project architect")
        );
        assert!(
            fs::read_to_string(&architect.skills[0])
                .unwrap()
                .contains("# Architecture")
        );
        assert_eq!(
            fs::read_to_string(&designer.context).unwrap(),
            "Exact user custom designer instructions"
        );
        assert_eq!(
            fs::read_to_string(&designer.skills[0]).unwrap(),
            "Exact custom design skill"
        );
        assert_eq!(upgrade_defaults(&path).unwrap(), 0);
    }
    #[test]
    fn init_roundtrip_and_preservation() {
        let dir = tempfile::tempdir().unwrap();
        let path = init(dir.path(), Provider::Codex).unwrap();
        let mut c = Config::load(&path).unwrap();
        assert_eq!(c.agents.len(), 6);
        fs::write(&c.agents[0].context, "custom").unwrap();
        assert!(init(dir.path(), Provider::Claude).is_err());
        assert_eq!(
            c.agents[0].instructions().unwrap().split('\n').next(),
            Some("custom")
        );
        c.agents[0].name = "lead".into();
        assert!(c.validate().is_err());
    }
}
