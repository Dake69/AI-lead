use std::path::Path;

pub const START: &str = "<!-- ai-lead:attached-skills:start -->";
pub const END: &str = "<!-- ai-lead:attached-skills:end -->";

pub fn base(role: &str) -> String {
    let role = match role {
        "lead" => include_str!("prompts/lead.md"),
        "frontend" => include_str!("prompts/frontend.md"),
        "backend" => include_str!("prompts/backend.md"),
        "architect" | "devops" => include_str!("prompts/architect.md"),
        "tester" => include_str!("prompts/tester.md"),
        "reviewer" => include_str!("prompts/reviewer.md"),
        _ => include_str!("prompts/designer.md"),
    };
    format!("{}\n\n{role}", include_str!("prompts/common.md"))
}

/// Exact previous built-in text, used only to identify safe upgrades. A single
/// user edit makes a context custom and prevents automatic replacement.
pub fn previous_base(role: &str) -> String {
    let role = match role {
        "lead" => include_str!("prompts/previous/lead.md"),
        "frontend" => include_str!("prompts/previous/frontend.md"),
        "backend" => include_str!("prompts/previous/backend.md"),
        "architect" | "devops" => include_str!("prompts/previous/devops.md"),
        "tester" => include_str!("prompts/previous/tester.md"),
        "reviewer" => include_str!("prompts/previous/reviewer.md"),
        _ => include_str!("prompts/previous/designer.md"),
    };
    format!("{}\n\n{role}", include_str!("prompts/previous/common.md"))
}

pub fn is_previous_base(role: &str, text: &str) -> bool {
    let previous = previous_base(role);
    // Older generators omitted the template's final newline. Compare known
    // generated variants rather than trimming potentially custom user text.
    let matches = |candidate: &str| {
        text == candidate || candidate.strip_suffix('\n').is_some_and(|old| text == old)
    };
    let parallel_role = match role {
        "lead" => include_str!("prompts/previous_parallel/lead.md"),
        "frontend" => include_str!("prompts/previous_parallel/frontend.md"),
        "backend" => include_str!("prompts/previous_parallel/backend.md"),
        "architect" | "devops" => include_str!("prompts/previous_parallel/architect.md"),
        "tester" => include_str!("prompts/previous_parallel/tester.md"),
        "reviewer" => include_str!("prompts/previous_parallel/reviewer.md"),
        _ => include_str!("prompts/previous_parallel/designer.md"),
    };
    let parallel = format!(
        "{}\n\n{parallel_role}",
        include_str!("prompts/previous_parallel/common.md")
    );
    matches(&parallel)
        || (role == "lead" && matches(&parallel.replace("src/design/ tokens/primitives", "src/design/** tokens/primitives")))
        || matches(&previous)
        || (role == "lead"
            && matches(&previous.replace(
                "Delegate implementation to the available specialists. Follow the current runtime permission mode: you are read-only in restricted sessions and may use file, shell and network tools in an explicitly authorized Full access session.",
                "Delegate implementation to the available specialists and remain read-only yourself.",
            )))
}

pub fn is_previous_skill(role: &str, text: &str) -> bool {
    let parallel = match role {
        "lead" => include_str!("prompts/previous_parallel/lead-skill.md"),
        "frontend" => include_str!("prompts/previous_parallel/frontend-skill.md"),
        "backend" => include_str!("prompts/previous_parallel/backend-skill.md"),
        "architect" | "devops" => include_str!("prompts/previous_parallel/architect-skill.md"),
        "tester" => include_str!("prompts/previous_parallel/tester-skill.md"),
        "reviewer" => include_str!("prompts/previous_parallel/reviewer-skill.md"),
        _ => include_str!("prompts/previous_parallel/designer-skill.md"),
    };
    text == parallel || previous_skill(role).is_some_and(|previous| text == previous)
}

/// Appended after custom context, skills and permissions on every invocation.
/// Role responsibilities remain separate from the user's full-access capability.
pub fn role_boundary(role: &str) -> &'static str {
    match role {
        "lead" => {
            "Lead: delegate and coordinate only; write no project code, tests or configuration. Delegate architecture decisions to architect, all tests/builds/browser verification to tester, and logic review to reviewer. Schedule tester and reviewer alongside implementation, not only at the end. Once their current evidence supports delivery, launch the project with the provided startup command and report the actual outcome."
        }
        "backend" => {
            "Backend: write backend production code only. Do not write/edit tests, fixtures, test tooling/configuration or frontend code. Tester owns test writing, test/build execution and browser checks. Send inspectable code paths and contract updates to tester and reviewer during implementation; fix only your own backend defects."
        }
        "frontend" => {
            "Frontend: implement user-facing web/mobile/desktop/terminal applications and client integration only. Do not write/edit tests, fixtures, test tooling/configuration or backend code. Tester owns tests, builds and browser/visual verification. Coordinate separate visual ownership with designer and publish inspectable code to reviewer/tester during implementation."
        }
        "tester" => {
            "Tester: alone author/edit tests, fixtures, test harness/configuration; run all automated tests, format/lint/type checks, builds and browser/native/visual acceptance. Start contract-based test writing alongside implementation. Report failures live; do not patch production code or visual components. Rerun final checks after relevant changes and supply lead the final startup command/URL."
        }
        "reviewer" => {
            "Reviewer: read-only logic review across all affected code, including tests, during implementation and again after fixes. Send findings live to owner and lead. Do not modify production files/tests/configuration or take over tester's test/build/browser workload. Use bounded passes over actual files; do not wait indefinitely for queued tasks or approve later unseen changes."
        }
        "architect" | "devops" => {
            "Architect: own stack/topology decisions, readable module/directory architecture, contracts and assigned structural/configuration artifacts. Do not implement backend/frontend features or write tests. Tester owns test/build/browser execution; reviewer owns independent logic review. Publish decisions promptly so implementation can proceed in parallel."
        }
        "designer" => {
            "Designer: implement visual tokens/styles and agreed visual primitives only, in separate owned paths with frontend. Do not implement backend/application behavior or write tests. Tester owns builds and browser/visual checks; provide design criteria and assess their evidence. Send inspectable visual code to reviewer during implementation."
        }
        _ => {
            "Stay within your configured specialist role; do not assume another specialist's responsibility. Ask the lead to reassign misrouted work."
        }
    }
}

pub fn previous_skill(role: &str) -> Option<&'static str> {
    match role {
        "lead" => Some(
            "# Orchestration\nStart with discovery and design when needed. Order dependent assignments deliberately. Specialists execute sequentially in one shared workspace. Include exact scope, expected artifact and verification in tasks. Reports and peer messages arrive at round boundaries. Reassign fixes after review. Finish with delivered behavior, actual validation and remaining limitations.",
        ),
        "designer" => Some(
            "# Design\nInspect existing design conventions. Specify hierarchy, spacing, typography, colors and component states. Include keyboard navigation, contrast and narrow layouts. Deliver implementation-ready decisions rather than generic advice.",
        ),
        "architect" | "devops" => Some(
            "# Operations\nPrefer reproducible builds. Keep secrets outside tracked files. Document environment variables and startup checks. Verify configuration locally. Report deployment requirements and unresolved operational risks.",
        ),
        _ => None,
    }
}

pub fn without_skills(text: &str) -> String {
    let mut text = text.to_owned();
    while let Some(start) = text.find(START) {
        let Some(end) = text[start..].find(END) else {
            break;
        };
        let begin = if text[..start].ends_with("\n\n") {
            start - 2
        } else {
            start
        };
        let mut finish = start + end + END.len();
        if text[finish..].starts_with('\n') {
            finish += 1;
        }
        text.replace_range(begin..finish, "");
    }
    text
}

pub fn skill_name(path: &Path) -> String {
    if path.file_name().is_some_and(|f| f == "SKILL.md") {
        path.parent()
            .and_then(Path::file_name)
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    } else {
        path.file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    }
}

pub fn with_skills(text: &str, skills: &[std::path::PathBuf]) -> String {
    let mut out = without_skills(text);
    out.push_str(&format!(
        "\n\n{START}\n## Attached skills — managed by AI Lead\n"
    ));
    if skills.is_empty() {
        out.push_str(
            "No skills are attached to this agent. Do not claim to have used an attached skill.\n",
        );
    } else {
        out.push_str("Read all attached skills before starting. Use every attached skill for the part of the task it governs, following its instructions and referenced resources. Report which skills you applied. If a skill is not applicable or cannot be used, explain the reason to the lead; do not silently ignore it.\n\n");
        for (index, skill) in skills.iter().enumerate() {
            out.push_str(&format!(
                "{}. {} — {}\n",
                index + 1,
                skill_name(skill),
                skill.display()
            ));
        }
    }
    out.push_str(END);
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn previous_templates_with_legacy_missing_newline_upgrade_without_trimming_custom_text() {
        for role in [
            "lead", "frontend", "backend", "designer", "devops", "tester", "reviewer",
        ] {
            let previous = previous_base(role);
            assert!(is_previous_base(role, &previous));
            assert!(is_previous_base(role, previous.strip_suffix('\n').unwrap()));
            assert!(!is_previous_base(role, &format!("{previous} ")));
            assert!(!is_previous_base(
                role,
                &format!("{previous}Custom requirement")
            ));
        }
        let older_lead = previous_base("lead").replace(
            "Delegate implementation to the available specialists. Follow the current runtime permission mode: you are read-only in restricted sessions and may use file, shell and network tools in an explicitly authorized Full access session.",
            "Delegate implementation to the available specialists and remain read-only yourself.",
        );
        assert!(is_previous_base(
            "lead",
            older_lead.strip_suffix('\n').unwrap()
        ));
    }
    #[test]
    fn skills_block_is_replaceable_and_does_not_duplicate() {
        let first = with_skills("Custom strict prompt", &["first.md".into()]);
        let second = with_skills(&first, &["my/second/SKILL.md".into()]);
        assert_eq!(second.matches(START).count(), 1);
        assert!(!second.contains("first.md"));
        assert!(second.contains("1. second"));
        assert_eq!(without_skills(&second), "Custom strict prompt");
    }
    #[test]
    fn managed_skills_preserve_the_exact_custom_prompt() {
        let custom = "Строгий промпт\n  preserve indentation  \n\n";
        let effective = with_skills(custom, &["rules.md".into()]);
        assert_eq!(without_skills(&effective), custom);
        assert_eq!(with_skills(&effective, &["rules.md".into()]), effective);
    }
    #[test]
    fn each_base_has_role_depth_and_persistent_memory_instructions() {
        for role in [
            "lead", "frontend", "backend", "designer", "devops", "tester", "reviewer",
        ] {
            let text = base(role);
            assert!(text.len() > 5500, "{role} prompt is too short");
            assert!(text.contains("next_steps"));
            assert!(text.contains("attached"));
        }
    }
}
