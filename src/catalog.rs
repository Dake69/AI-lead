//! Provider metadata discovery only: no prompts, turns or inference requests.
use crate::config::{Agent, Provider};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::HashSet, path::PathBuf, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::Command,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Model {
    pub id: String,
    pub name: String,
    pub description: String,
    pub efforts: Vec<String>,
    pub recommended: bool,
}
#[derive(Clone, Debug)]
pub struct Catalog {
    pub models: Vec<Model>,
    pub source: String,
    pub note: String,
}
pub fn default_model() -> Model {
    Model {
        id: String::new(),
        name: "Default for your account".into(),
        description: "Let the installed CLI choose its configured model".into(),
        efforts: vec![],
        recommended: false,
    }
}
fn clean(s: &str, limit: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(limit).collect()
}
fn name(id: &str) -> String {
    id.rsplit('/')
        .next()
        .unwrap_or(id)
        .split(['-', '_'])
        .map(|part| {
            if part.eq_ignore_ascii_case("gpt") {
                "GPT".into()
            } else {
                let mut chars = part.chars();
                chars
                    .next()
                    .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
                    .unwrap_or_default()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
pub fn parse_model(value: &Value) -> Option<Model> {
    if value["hidden"] == true
        || value["active"] == false
        || value["visibility"] == "hide"
        || value["visibility"] == "hidden"
        || value["lifecycle"] == "retired"
    {
        return None;
    }
    let id = ["model", "slug", "value", "id"]
        .iter()
        .find_map(|key| value[*key].as_str())?;
    if id.is_empty() || id.len() > 512 || id.chars().any(char::is_control) {
        return None;
    }
    // Exclude models explicitly unsuitable for this tool-calling coding team.
    if value["capabilities"]["tool_calling"] == false
        || value["capabilities"]["function_calling"] == false
    {
        return None;
    }
    if let Some(parameters) = value["supported_parameters"].as_array()
        && !parameters
            .iter()
            .any(|p| p == "tools" || p == "tool_choice")
    {
        return None;
    }
    if let Some(outputs) = value["architecture"]["output_modalities"].as_array()
        && !outputs.iter().any(|p| p == "text")
    {
        return None;
    }
    let lower = id.to_lowercase();
    if [
        "text-embedding",
        "embedding-",
        "whisper-",
        "tts-",
        "dall-e-",
        "gpt-image-",
        "imagen-",
        "veo-",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
    {
        return None;
    }
    let title = ["displayName", "display_name", "name"]
        .iter()
        .find_map(|key| value[*key].as_str())
        .filter(|s| !s.trim().is_empty())
        .map(|s| clean(s, 160))
        .unwrap_or_else(|| name(id));
    let levels = value
        .get("supportedReasoningEfforts")
        .or_else(|| value.get("supported_reasoning_levels"))
        .or_else(|| value.get("supportedEffortLevels"))
        .and_then(Value::as_array);
    let efforts = levels
        .into_iter()
        .flatten()
        .filter_map(|level| {
            level
                .as_str()
                .or_else(|| level["reasoningEffort"].as_str())
                .or_else(|| level["effort"].as_str())
        })
        .filter(|s| ["minimal", "low", "medium", "high", "xhigh", "max", "ultra"].contains(s))
        .map(str::to_owned)
        .collect();
    Some(Model {
        id: id.into(),
        name: title,
        description: clean(value["description"].as_str().unwrap_or(""), 600),
        efforts,
        recommended: value["isDefault"] == true,
    })
}
pub fn parse_rows(rows: &[Value]) -> Vec<Model> {
    let mut seen = HashSet::new();
    rows.iter()
        .filter_map(parse_model)
        .filter(|m| seen.insert(m.id.clone()))
        .collect()
}

pub async fn discover(agent: &Agent) -> Result<Catalog> {
    if agent.provider.is_api() {
        let models =
            tokio::time::timeout(Duration::from_secs(15), crate::api::model_catalog(agent))
                .await
                .context("Model catalog timed out; check the connection and retry")??;
        return Ok(Catalog { models, source: format!("Live {} catalog", agent.provider.label()), note: "Catalog listing does not run a model. Model access and tool support are checked on invocation.".into() });
    }
    let result = tokio::time::timeout(Duration::from_secs(10), cli_models(agent)).await;
    match result {
        Ok(Ok(models)) if !models.is_empty() => Ok(Catalog { models, source: format!("Live {} catalog", agent.provider.label()), note: "Reported by the installed CLI. Availability depends on your account and provider.".into() }),
        _ if agent.provider == Provider::Codex => {
            let home = std::env::var_os("CODEX_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".codex")));
            if let Some(home) = home {
                let path = home.join("models_cache.json");
                if let Ok(file) = tokio::fs::File::open(path).await {
                    let mut bytes = Vec::new();
                    file.take(8 * 1024 * 1024 + 1).read_to_end(&mut bytes).await?;
                    if bytes.len() <= 8 * 1024 * 1024
                        && let Ok(value) = serde_json::from_slice::<Value>(&bytes)
                        && let Some(rows) = value["models"].as_array() {
                        let models = parse_rows(rows);
                        if !models.is_empty() { return Ok(Catalog { models, source: "Local Codex cache (offline)".into(), note: "Live discovery unavailable. Cached models may be outdated; refresh after signing into Codex.".into() }); }
                    }
                }
            }
            bail!("Cannot load the Codex catalog. Install/sign into Codex and refresh. You can still select Default for your account.");
        },
        _ => Ok(Catalog { models: [
            ("best", "Best available Claude", "Use the CLI's best-model alias"),
            ("fable", "Claude Fable", "Long and demanding tasks"),
            ("opus", "Claude Opus", "Complex reasoning and coding"),
            ("sonnet", "Claude Sonnet", "Everyday coding tasks"),
            ("haiku", "Claude Haiku", "Fast, lighter tasks"),
            ("sonnet[1m]", "Claude Sonnet · extended context", "Extended context where supported"),
            ("opus[1m]", "Claude Opus · extended context", "Extended context where supported"),
            ("opusplan", "Claude Opus plan / Sonnet execute", "Claude's combined planning mode"),
        ].into_iter().map(|(id, name, description)| Model { id: id.into(), name: name.into(), description: description.into(), efforts: vec![], recommended: false }).collect(), source: "Claude Code aliases (live discovery unavailable)".into(), note: "Standard CLI aliases, not an account entitlement check. Install/sign into Claude Code and refresh for its actual catalog.".into() }),
    }
}

async fn send(stdin: &mut tokio::process::ChildStdin, value: Value) -> Result<()> {
    let mut bytes = serde_json::to_vec(&value)?;
    bytes.push(b'\n');
    stdin.write_all(&bytes).await?;
    stdin.flush().await?;
    Ok(())
}
async fn frame(reader: &mut BufReader<tokio::process::ChildStdout>) -> Result<Value> {
    let mut line = Vec::new();
    loop {
        let bytes = reader.fill_buf().await?;
        if bytes.is_empty() {
            bail!("CLI closed before returning its model catalog");
        }
        let count = bytes
            .iter()
            .position(|b| *b == b'\n')
            .map_or(bytes.len(), |n| n + 1);
        if line.len() + count > 4 * 1024 * 1024 {
            bail!("CLI catalog frame exceeds 4 MiB");
        }
        let complete = bytes[count - 1] == b'\n';
        line.extend_from_slice(&bytes[..count]);
        reader.consume(count);
        if complete {
            return serde_json::from_slice(&line).context("Invalid CLI catalog frame");
        }
    }
}
async fn cli_models(agent: &Agent) -> Result<Vec<Model>> {
    cli_models_using(
        agent,
        std::path::Path::new(if agent.provider == Provider::Codex {
            "codex"
        } else {
            "claude"
        }),
    )
    .await
}

async fn cli_models_using(agent: &Agent, executable: &std::path::Path) -> Result<Vec<Model>> {
    let codex = agent.provider == Provider::Codex;
    let mut command = Command::new(executable);
    if codex {
        command.args(["app-server", "--listen", "stdio://"]);
    } else {
        command.args([
            "--print",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--no-session-persistence",
        ]);
        command.env_remove("CLAUDECODE");
    }
    // Discovery never inherits the project directory or sends user turns.
    command
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command.spawn().context("Installed CLI is unavailable")?;
    #[cfg(unix)]
    let _group = crate::provider::ProcessGroup(child.id().context("Missing catalog process ID")?);
    let mut stdin = child.stdin.take().context("Missing CLI stdin")?;
    let mut reader = BufReader::new(child.stdout.take().context("Missing CLI stdout")?);
    let result = async {
        if !codex {
            send(&mut stdin, json!({"type":"control_request","request_id":"ai-lead-catalog","request":{"subtype":"initialize","hooks":null}})).await?;
            for _ in 0..200 {
                let value = frame(&mut reader).await?;
                if value["type"] == "control_response" && value["response"]["request_id"] == "ai-lead-catalog" {
                    let rows = value["response"]["response"]["models"].as_array().context("Claude did not return model metadata")?;
                    return Ok(parse_rows(rows));
                }
            }
            bail!("Claude model discovery exceeded its frame limit");
        }
        send(&mut stdin, json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"ai-lead","version":env!("CARGO_PKG_VERSION")}}})).await?;
        let mut initialized = false;
        for _ in 0..200 {
            let value = frame(&mut reader).await?;
            if value["id"] == 1 {
                if value.get("error").is_some() { bail!("Codex catalog initialization failed"); }
                initialized = true;
                break;
            }
        }
        if !initialized { bail!("Codex initialization response not received"); }
        send(&mut stdin, json!({"method":"initialized"})).await?;
        let mut models = Vec::new();
        let mut seen_models = HashSet::new();
        let mut cursor = Value::Null;
        let mut seen_cursors = HashSet::new();
        for page in 0..100 {
            let id = page + 2;
            send(&mut stdin, json!({"id":id,"method":"model/list","params":{"limit":100,"includeHidden":false,"cursor":cursor}})).await?;
            let mut response = None;
            for _ in 0..200 { let value = frame(&mut reader).await?; if value["id"] == id { response = Some(value); break; } }
            let value = response.context("Codex catalog response not received")?;
            let rows = value["result"]["data"].as_array().context("Codex model catalog unavailable")?;
            models.extend(parse_rows(rows).into_iter().filter(|model| seen_models.insert(model.id.clone())));
            cursor = value["result"]["nextCursor"].clone();
            if cursor.is_null() { return Ok(models); }
            if !seen_cursors.insert(cursor.to_string()) { bail!("Repeated Codex catalog cursor"); }
        }
        bail!("Codex model catalog exceeded 100 pages")
    }.await;
    let _ = child.kill().await;
    let _ = child.wait().await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_keeps_friendly_names_exact_ids_and_efforts_and_filters_explicitly_unsuitable_models()
     {
        let rows = json!([
            {"model":"internal-v7","displayName":"Friendly Coding Model","description":"Works with code","isDefault":true,"supportedReasoningEfforts":[{"reasoningEffort":"medium"},{"reasoningEffort":"xhigh"}]},
            {"model":"internal-v7","displayName":"Duplicate"},
            {"id":"hidden-model","hidden":true},
            {"id":"gpt-image-test"},
            {"id":"retired","lifecycle":"retired"},
            {"id":"no-tools","supported_parameters":["temperature"]},
            {"id":"tool-model","name":"Tool Model","supported_parameters":["tools"]},
            {"value":"claude-internal","displayName":"Friendly Claude","supportedEffortLevels":["low","high"]}
        ]);
        let models = parse_rows(rows.as_array().unwrap());
        assert_eq!(models.len(), 3);
        assert_eq!(models[0].name, "Friendly Coding Model");
        assert_eq!(models[0].id, "internal-v7");
        assert_eq!(models[0].efforts, ["medium", "xhigh"]);
        assert!(models[0].recommended);
        assert_eq!(models[2].efforts, ["low", "high"]);
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn codex_and_claude_catalog_handshakes_send_no_inference_turns() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let config =
            crate::config::Config::load(&crate::config::init(dir.path(), Provider::Codex).unwrap())
                .unwrap();
        let executable = dir.path().join("catalog-cli");
        std::fs::write(&executable, r#"#!/usr/bin/env python3
import sys,json
def read(): return json.loads(sys.stdin.readline())
def write(v): print(json.dumps(v),flush=True)
if 'app-server' in sys.argv:
    init=read(); assert init['method']=='initialize'
    write({'id':1,'result':{}})
    assert read()['method']=='initialized'
    request=read(); assert request['method']=='model/list'
    write({'id':request['id'],'result':{'data':[{'model':'server-1','displayName':'First friendly model'}],'nextCursor':'second'}})
    request=read(); assert request['method']=='model/list' and request['params']['cursor']=='second'
    write({'id':request['id'],'result':{'data':[{'model':'server-2','displayName':'Second friendly model'}],'nextCursor':None}})
else:
    request=read(); assert request['request']['subtype']=='initialize' and request['type']=='control_request'
    write({'type':'control_response','response':{'request_id':request['request_id'],'subtype':'success','response':{'models':[{'value':'claude-alias','displayName':'Friendly Claude'}]}}})
sys.stdin.read()
"#).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut agent = config.leader;
        let codex = tokio::time::timeout(
            Duration::from_secs(3),
            cli_models_using(&agent, &executable),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            codex.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["server-1", "server-2"]
        );
        agent.provider = Provider::Claude;
        let claude = tokio::time::timeout(
            Duration::from_secs(3),
            cli_models_using(&agent, &executable),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(claude[0].name, "Friendly Claude");
        assert_eq!(claude[0].id, "claude-alias");
    }
}
