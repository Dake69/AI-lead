//! Explicit project-scoped consent; a TOML setting alone never grants full access.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub const EXPLANATION: &str = "Full access applies to EVERY agent, including the lead. Agents can run shell commands, install software, access the network, and read/change/delete files outside this project using your OS account and available credentials. Provider permission prompts and AI Lead's project file restrictions are disabled. This does not grant root/admin rights or override provider/OS policies. Only actions within your task are authorized.";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Consent {
    version: u32,
    project: PathBuf,
    accepted_at: u64,
}
fn file(project: &Path) -> PathBuf {
    project.join(".ai-lead/permissions.json")
}
pub fn granted(project: &Path) -> Result<bool> {
    let path = file(project);
    let value = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    let Ok(consent) = serde_json::from_slice::<Consent>(&value) else {
        return Ok(false);
    };
    Ok(consent.version == 1 && consent.project == project.canonicalize()?)
}
pub fn grant(project: &Path) -> Result<()> {
    let root = project.join(".ai-lead");
    fs::create_dir_all(&root)?;
    let ignore = root.join(".gitignore");
    let mut content = match fs::read_to_string(&ignore) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    if !content.lines().any(|l| l.trim() == "/permissions.json") {
        content.push_str("\n/permissions.json\n");
        fs::write(ignore, content)?;
    }
    let consent = Consent {
        version: 1,
        project: project.canonicalize()?,
        accepted_at: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
    };
    let temporary = root.join(format!(".permission-{}.tmp", uuid::Uuid::new_v4()));
    fs::write(&temporary, serde_json::to_vec_pretty(&consent)?)?;
    if let Err(error) = fs::rename(&temporary, file(project)) {
        let _ = fs::remove_file(temporary);
        return Err(error.into());
    }
    Ok(())
}
pub fn revoke(project: &Path) -> Result<()> {
    match fs::remove_file(file(project)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}
pub fn confirm_cli(project: &Path, yes: bool) -> Result<bool> {
    if granted(project)? {
        return Ok(true);
    }
    if yes {
        grant(project)?;
        return Ok(true);
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!(
            "Full access requires project consent. Run `ai-lead permissions full-access` interactively, or explicitly authorize with `ai-lead permissions full-access --yes`. No agents were started."
        );
    }
    println!("FULL ACCESS — {}\n\n{}\n", project.display(), EXPLANATION);
    print!(
        "Allow full access for all agents in this project? Type ALLOW (anything else cancels): "
    );
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .context("Cannot read permission response")?;
    if answer.trim() == "ALLOW" {
        grant(project)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn consent_is_explicit_project_scoped_revocable_and_not_cloneable() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        assert!(!granted(a.path()).unwrap());
        grant(a.path()).unwrap();
        assert!(granted(a.path()).unwrap());
        fs::create_dir(b.path().join(".ai-lead")).unwrap();
        fs::copy(file(a.path()), file(b.path())).unwrap();
        assert!(!granted(b.path()).unwrap());
        assert!(
            fs::read_to_string(a.path().join(".ai-lead/.gitignore"))
                .unwrap()
                .contains("/permissions.json")
        );
        revoke(a.path()).unwrap();
        assert!(!granted(a.path()).unwrap());
    }
}
