use crate::config::Config;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MemoryUpdate {
    pub facts: Vec<String>,
    pub next_steps: Vec<String>,
    pub blockers: Vec<String>,
    pub shared: Vec<String>,
}
impl MemoryUpdate {
    pub fn validate(&self) -> Result<()> {
        for list in [&self.facts, &self.next_steps, &self.blockers, &self.shared] {
            if list.len() > 64 || list.iter().any(|s| s.chars().count() > 4000) {
                bail!(
                    "Memory updates are limited to 64 entries per field and 4000 characters per entry"
                );
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub to: String,
    pub body: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Assignment {
    pub agent: String,
    pub task: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// None means legacy/exclusive root ownership; [] means read-only.
    #[serde(default)]
    pub files: Option<Vec<String>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decision {
    #[serde(default)]
    pub brief: String,
    #[serde(default)]
    pub verification_limits: Vec<String>,
    pub action: String,
    pub assignments: Vec<Assignment>,
    pub messages: Vec<Message>,
    pub summary: String,
    #[serde(default)]
    pub memory: MemoryUpdate,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    #[serde(default)]
    pub verification_limits: Vec<String>,
    pub summary: String,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub memory: MemoryUpdate,
}

impl Decision {
    pub fn normalize_roles(&mut self) {
        for assignment in &mut self.assignments {
            // Legacy plans did not carry ownership. Review never needs it.
            if assignment.agent == "reviewer" && assignment.files.is_none() {
                assignment.files = Some(Vec::new());
            }
        }
    }

    pub fn validate(&self, config: &Config) -> Result<()> {
        if self.summary.trim().is_empty() {
            bail!("Leader summary cannot be empty");
        }
        if !["delegate", "finish", "finish_with_limits", "blocked"].contains(&self.action.as_str())
        {
            bail!("Unknown leader action: {}", self.action);
        }
        if self.action != "delegate" && !self.assignments.is_empty() {
            bail!("A terminal decision cannot include assignments");
        }
        if self.action == "delegate" && self.assignments.is_empty() && self.messages.is_empty() {
            bail!("Empty delegation");
        }
        if self.action == "finish_with_limits" && self.verification_limits.is_empty() {
            bail!("finish_with_limits requires explicit outstanding verification_limits");
        }
        if self.action == "finish" && !self.verification_limits.is_empty() {
            bail!("Use finish_with_limits when verification remains unavailable");
        }
        validate_limits(&self.verification_limits)?;
        if self.brief.len() > 96_000 {
            bail!("Shared brief exceeds 96 KiB");
        }
        let mut assigned = std::collections::HashSet::new();
        for a in &self.assignments {
            if !assigned.insert(a.agent.as_str()) {
                bail!(
                    "Only one assignment per agent is allowed in a plan: {}",
                    a.agent
                );
            }
        }
        for a in &self.assignments {
            if !config.agents.iter().any(|x| x.name == a.agent) {
                bail!("Unknown specialist: {}", a.agent);
            }
            if a.task.trim().is_empty() {
                bail!("Empty task for {}", a.agent);
            }
        }
        for a in &self.assignments {
            for dependency in &a.depends_on {
                if dependency == &a.agent || !assigned.contains(dependency.as_str()) {
                    bail!("Invalid dependency {dependency} for {}", a.agent);
                }
            }
            if let Some(paths) = &a.files {
                if a.agent == "reviewer" && !paths.is_empty() {
                    bail!(
                        "Reviewer performs read-only logic review; assign files=[] and route fixes to their author"
                    );
                }
                if paths.len() > 64 {
                    bail!("Too many file ownership paths");
                }
                for path in paths {
                    validate_scope(path)?;
                    if a.agent != "tester" && is_test_scope(path) {
                        bail!(
                            "Only tester may own test code/fixtures/configuration: {} assigned to {}",
                            path,
                            a.agent
                        );
                    }
                }
            }
        }
        let mut resolved = std::collections::HashSet::new();
        loop {
            let before = resolved.len();
            for a in &self.assignments {
                if a.depends_on.iter().all(|d| resolved.contains(d.as_str())) {
                    resolved.insert(a.agent.as_str());
                }
            }
            if before == resolved.len() {
                break;
            }
        }
        if resolved.len() != self.assignments.len() {
            bail!("Assignment dependency cycle");
        }
        self.memory.validate()?;
        validate_messages(&self.messages, config)
    }
}

fn is_test_scope(path: &str) -> bool {
    std::path::Path::new(path).components().any(|part| {
        let s = part.as_os_str().to_string_lossy().to_ascii_lowercase();
        matches!(
            s.as_str(),
            "test"
                | "tests"
                | "__tests__"
                | "fixtures"
                | "__fixtures__"
                | "e2e"
                | "conftest.py"
                | "pytest.ini"
        ) || s.contains(".test.")
            || s.contains(".spec.")
            || s.starts_with("test_")
            || s.ends_with("_test.go")
            || [
                "playwright.config.",
                "vitest.config.",
                "jest.config.",
                "cypress.config.",
            ]
            .iter()
            .any(|prefix| s.starts_with(prefix))
    })
}

pub fn validate_limits(limits: &[String]) -> Result<()> {
    if limits.len() > 64
        || limits
            .iter()
            .any(|s| s.trim().is_empty() || s.len() > 16_000)
    {
        bail!("verification_limits must contain up to 64 nonempty descriptions (16 KiB each)");
    }
    Ok(())
}
fn validate_scope(path: &str) -> Result<()> {
    if path.is_empty()
        || path.len() > 4096
        || path.contains(['*', '?', '\\'])
        || std::path::Path::new(path).components().any(|c| {
            !matches!(
                c,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        })
    {
        bail!(
            "Ownership paths must be project-relative file/directory prefixes, without globs or parent traversal: {path}"
        );
    }
    Ok(())
}
/// Ancestor/descendant prefixes conflict; unspecified legacy scopes own the whole project.
pub fn scopes_conflict(a: &Assignment, b: &Assignment) -> bool {
    if a.files.as_ref().is_some_and(Vec::is_empty) || b.files.as_ref().is_some_and(Vec::is_empty) {
        return false;
    }
    let (Some(a), Some(b)) = (&a.files, &b.files) else {
        return true;
    };
    a.iter().any(|a| {
        b.iter().any(|b| {
            let clean = |s: &str| {
                std::path::Path::new(s)
                    .components()
                    .filter(|c| !matches!(c, std::path::Component::CurDir))
                    .collect::<std::path::PathBuf>()
            };
            let a = clean(a);
            let b = clean(b);
            a.starts_with(&b) || b.starts_with(&a)
        })
    })
}

pub fn validate_messages(messages: &[Message], config: &Config) -> Result<()> {
    if messages.len() > 64 {
        bail!("At most 64 messages per response are allowed");
    }
    for m in messages {
        if m.body.len() > 16 * 1024 {
            bail!("Message exceeds 16 KiB");
        }
        if m.body.trim().is_empty() {
            bail!("Empty message");
        }
        if m.to != config.leader.name && !config.agents.iter().any(|a| a.name == m.to) {
            bail!("Unknown message recipient: {}", m.to);
        }
    }
    Ok(())
}

pub fn schema(leader: bool) -> Value {
    let message = json!({"type":"object", "additionalProperties":false,
        "properties":{"to":{"type":"string"},"body":{"type":"string"}}, "required":["to","body"]});
    let mut properties = json!({"summary":{"type":"string"}, "messages":{"type":"array", "items":message},"verification_limits":{"type":"array","items":{"type":"string"}}});
    properties["memory"] = json!({"type":"object","additionalProperties":false,
        "properties":{"facts":{"type":"array","items":{"type":"string"}},"next_steps":{"type":"array","items":{"type":"string"}},"blockers":{"type":"array","items":{"type":"string"}},"shared":{"type":"array","items":{"type":"string"}}},
        "required":["facts","next_steps","blockers","shared"]});
    let required = if leader {
        properties["action"] =
            json!({"type":"string", "enum":["delegate","finish","finish_with_limits","blocked"]});
        properties["brief"] = json!({"type":"string","description":"Shared architecture, chosen stack, API contract, UI design brief and ownership; visible verbatim to every specialist."});
        properties["assignments"] = json!({"type":"array", "items":{"type":"object", "additionalProperties":false,
            "properties":{"agent":{"type":"string"},"task":{"type":"string"},"depends_on":{"type":"array","items":{"type":"string"}},"files":{"type":["array","null"],"items":{"type":"string"},"description":"Owned file/directory prefixes relative to project; disjoint paths run concurrently. [] means no code writes. null reserves entire project."}}, "required":["agent","task","depends_on","files"]}});
        vec![
            "summary",
            "messages",
            "action",
            "assignments",
            "memory",
            "brief",
            "verification_limits",
        ]
    } else {
        vec!["summary", "messages", "memory", "verification_limits"]
    };
    json!({"type":"object", "additionalProperties":false, "properties":properties, "required":required})
}

pub fn parse<T: serde::de::DeserializeOwned>(text: &str) -> Result<T> {
    let text = text.trim();
    let text = text
        .strip_prefix("```json")
        .or_else(|| text.strip_prefix("```"))
        .and_then(|t| t.trim().strip_suffix("```"))
        .unwrap_or(text);
    Ok(serde_json::from_str(text.trim())?)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn assignment(name: &str, files: Option<Vec<&str>>) -> Assignment {
        Assignment {
            agent: name.into(),
            task: "Implement owned part".into(),
            depends_on: vec![],
            files: files.map(|v| v.into_iter().map(str::to_owned).collect()),
        }
    }
    #[test]
    fn test_ownership_is_reserved_for_tester_and_reviewer_owns_no_product_files() {
        let dir = tempfile::tempdir().unwrap();
        let config =
            Config::load(&crate::config::init(dir.path(), crate::config::Provider::Codex).unwrap())
                .unwrap();
        let mut decision = Decision {
            brief: String::new(),
            verification_limits: vec![],
            action: "delegate".into(),
            assignments: vec![],
            messages: vec![],
            summary: "Role-specific work".into(),
            memory: Default::default(),
        };
        for path in [
            "tests/api.rs",
            "src/page.test.tsx",
            "test_api.py",
            "src/api_test.go",
            "playwright.config.ts",
            "__fixtures__/account.json",
        ] {
            for role in ["backend", "frontend", "designer", "architect"] {
                decision.assignments = vec![assignment(role, Some(vec![path]))];
                assert!(
                    decision
                        .validate(&config)
                        .unwrap_err()
                        .to_string()
                        .contains("Only tester"),
                    "{role}: {path}"
                );
            }
            decision.assignments = vec![assignment("tester", Some(vec![path]))];
            decision.validate(&config).unwrap();
        }
        decision.assignments = vec![assignment("reviewer", Some(vec!["src/"]))];
        assert!(
            decision
                .validate(&config)
                .unwrap_err()
                .to_string()
                .contains("read-only")
        );
        decision.assignments = vec![assignment("reviewer", None)];
        decision.normalize_roles();
        assert_eq!(decision.assignments[0].files, Some(vec![]));
        decision.validate(&config).unwrap();
        assert!(!scopes_conflict(
            &decision.assignments[0],
            &assignment("backend", None)
        ));
    }
    #[test]
    fn ownership_uses_normalized_path_boundaries_and_exclusive_legacy_scopes() {
        assert!(scopes_conflict(
            &assignment("a", Some(vec!["src"])),
            &assignment("b", Some(vec!["src/pages"]))
        ));
        assert!(!scopes_conflict(
            &assignment("a", Some(vec!["src"])),
            &assignment("b", Some(vec!["src-server"]))
        ));
        assert!(scopes_conflict(
            &assignment("a", Some(vec!["./"])),
            &assignment("b", Some(vec!["src/pages"]))
        ));
        assert!(!scopes_conflict(
            &assignment("a", Some(vec![])),
            &assignment("b", Some(vec!["src"]))
        ));
        assert!(scopes_conflict(
            &assignment("a", None),
            &assignment("b", Some(vec!["src"]))
        ));
        assert!(validate_scope("../outside").is_err());
        assert!(validate_scope("/tmp/file").is_err());
        assert!(validate_scope("src/**").is_err());
    }
    #[test]
    fn schema_exposes_shared_brief_dependencies_and_limited_delivery() {
        let schema = schema(true);
        assert!(
            schema["properties"]["action"]["enum"]
                .as_array()
                .unwrap()
                .contains(&json!("finish_with_limits"))
        );
        assert!(
            schema["properties"]["assignments"]["items"]["properties"]
                .get("depends_on")
                .is_some()
        );
        assert!(
            schema["required"]
                .as_array()
                .unwrap()
                .contains(&json!("brief"))
        );
    }
}
