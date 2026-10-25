//! Ollama models as opencode providers: find the models the owner runs (this machine and any host
//! given with `init --ollama-url`) and write one provider per host into `opencode.jsonc`.
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const LOCAL_URL: &str = "http://127.0.0.1:11434";
const DEFAULT_PORT: &str = "11434";
/// Cap for `limit.output`: opencode wants both limits, Ollama only reports the context length.
const MAX_OUTPUT: u64 = 8192;

#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    pub name: String,
    pub size: u64,
    pub context: Option<u64>,
    pub tools: bool,
    pub embedding: bool,
}

/// `host[:port]`, `http(s)://host[:port][/v1]` -> `scheme://host[:port]` (no trailing slash, no `/v1`).
/// `None` for anything with a path or another scheme.
pub fn normalize_url(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let full = if raw.contains("://") {
        raw.to_string()
    } else {
        format!("http://{raw}")
    };
    let trimmed = full.trim_end_matches('/');
    let trimmed = trimmed
        .strip_suffix("/v1")
        .unwrap_or(trimmed)
        .trim_end_matches('/');
    let (scheme, rest) = trimmed.split_once("://")?;
    if !matches!(scheme, "http" | "https") || rest.is_empty() || rest.contains('/') {
        return None;
    }
    let rest = rest
        .strip_prefix("0.0.0.0")
        .map_or(rest.to_string(), |tail| format!("127.0.0.1{tail}"));
    Some(format!("{scheme}://{rest}"))
}

fn authority(base: &str) -> &str {
    base.split_once("://").map_or(base, |(_, rest)| rest)
}

fn has_port(base: &str) -> bool {
    authority(base).contains(':')
}

/// Provider id in `opencode.jsonc`: `ollama` for this machine, `ollama-<host slug>` for the rest.
pub fn provider_id(base: &str) -> String {
    if base == LOCAL_URL {
        return "ollama".to_string();
    }
    let auth = authority(base);
    let auth = auth
        .strip_suffix(&format!(":{DEFAULT_PORT}"))
        .unwrap_or(auth);
    let slug: String = auth
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    format!("ollama-{}", slug.trim_matches('-'))
}

fn first_context(show: &Value) -> Option<u64> {
    show["model_info"]
        .as_object()?
        .iter()
        .find(|(k, _)| k.ends_with(".context_length"))
        .and_then(|(_, v)| v.as_u64())
}

/// One `/api/tags` entry plus its `/api/show` answer (absent when the call failed). Servers that do not
/// report `capabilities` are treated as chat models with tools unless the name says embedding.
pub fn parse_model(name: &str, size: u64, show: Option<&Value>) -> Model {
    let caps = show.and_then(|s| s["capabilities"].as_array());
    let has = |cap: &str| caps.is_some_and(|c| c.iter().any(|v| v == cap));
    let name_embeds = name.to_lowercase().contains("embed");
    let embedding = if caps.is_some() {
        has("embedding")
    } else {
        name_embeds
    };
    let tools = if caps.is_some() {
        has("tools")
    } else {
        !name_embeds
    };
    Model {
        name: name.to_string(),
        size,
        context: show.and_then(first_context),
        tools,
        embedding,
    }
}

fn http(url: &str, body: Option<&Value>) -> Result<Value, String> {
    let payload = body.map(Value::to_string);
    let mut args = vec!["-sS", "--fail", "--connect-timeout", "2", "-m", "5"];
    if let Some(p) = payload.as_deref() {
        args.extend(["-H", "Content-Type: application/json", "-d", p]);
    }
    args.push(url);
    let out = crate::sys::run_timeout("curl", &args, Some(std::time::Duration::from_secs(8)))?;
    serde_json::from_str(&out).map_err(|e| format!("{url}: not JSON ({e})"))
}

fn list_models(base: &str) -> Result<Vec<Model>, String> {
    let tags = http(&format!("{base}/api/tags"), None)?;
    let entries = tags["models"]
        .as_array()
        .ok_or_else(|| format!("{base}/api/tags: no `models` list"))?;
    Ok(entries
        .iter()
        .filter_map(|e| {
            let name = e["name"].as_str()?;
            let show = http(&format!("{base}/api/show"), Some(&json!({ "model": name }))).ok();
            Some(parse_model(
                name,
                e["size"].as_u64().unwrap_or(0),
                show.as_ref(),
            ))
        })
        .collect())
}

/// Ask the host for its models. A URL without a port that does not answer is tried again on `:11434`.
/// Returns the base URL that answered.
pub fn discover(base: &str) -> Result<(String, Vec<Model>), String> {
    let first = list_models(base);
    if let Ok(models) = first {
        return Ok((base.to_string(), models));
    }
    let err = first.unwrap_err();
    if has_port(base) {
        return Err(err);
    }
    let retry = format!("{base}:{DEFAULT_PORT}");
    list_models(&retry).map(|m| (retry, m)).map_err(|_| err)
}

fn model_entry(m: &Model) -> Value {
    let mut entry =
        json!({ "name": m.name, "tool_call": true, "cost": { "input": 0, "output": 0 } });
    if let Some(ctx) = m.context {
        entry["limit"] = json!({ "context": ctx, "output": ctx.min(MAX_OUTPUT) });
    }
    entry
}

/// The provider entry for `opencode.jsonc` (`provider.<id>`): only models that can call tools.
pub fn provider_entry(base: &str, models: &[Model]) -> Value {
    let chat: serde_json::Map<String, Value> = models
        .iter()
        .filter(|m| m.tools && !m.embedding)
        .map(|m| (m.name.clone(), model_entry(m)))
        .collect();
    json!({
        "npm": "@ai-sdk/openai-compatible",
        "name": format!("Ollama ({})", authority(base)),
        "options": { "baseURL": format!("{base}/v1") },
        "models": chat,
    })
}

/// Fill `ours` into `existing` without replacing anything the user edited: missing keys are added,
/// existing models are kept. Returns whether `existing` changed.
pub fn merge_provider(existing: &mut Value, ours: &Value) -> bool {
    let (Some(have), Some(want)) = (existing.as_object_mut(), ours.as_object()) else {
        return false;
    };
    let mut changed = false;
    for (key, value) in want {
        match have.get_mut(key) {
            None => {
                have.insert(key.clone(), value.clone());
                changed = true;
            }
            Some(slot) if key == "options" || key == "models" => {
                changed |= merge_missing(slot, value)
            }
            Some(_) => {}
        }
    }
    changed
}

/// Add the keys of `from` that `into` lacks (one level). Returns whether anything was added.
fn merge_missing(into: &mut Value, from: &Value) -> bool {
    let (Some(dst), Some(src)) = (into.as_object_mut(), from.as_object()) else {
        return false;
    };
    let missing: Vec<_> = src.iter().filter(|(k, _)| !dst.contains_key(*k)).collect();
    let changed = !missing.is_empty();
    dst.extend(missing.into_iter().map(|(k, v)| (k.clone(), v.clone())));
    changed
}

/// `<config>/a2a/ollama.json`: the extra hosts later runs re-check without flags.
pub fn hosts_file(root: &Path) -> PathBuf {
    root.join("a2a").join("ollama.json")
}

pub fn load_hosts(root: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(hosts_file(root)) else {
        return Vec::new();
    };
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    v["hosts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|h| h.as_str().and_then(normalize_url))
        .collect()
}

fn save_hosts(root: &Path, hosts: &[String]) -> Result<(), String> {
    let path = hosts_file(root);
    std::fs::create_dir_all(root.join("a2a"))
        .map_err(|e| format!("cannot create {}: {e}", root.join("a2a").display()))?;
    let text =
        serde_json::to_string_pretty(&json!({ "hosts": hosts })).map_err(|e| e.to_string())?;
    std::fs::write(&path, text + "\n").map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Every `--ollama-url <url>` (repeatable); `Err` names the value that is not a usable URL.
pub fn urls_from_args(args: &[String]) -> Result<Vec<String>, String> {
    args.iter()
        .enumerate()
        .filter(|(_, a)| *a == "--ollama-url")
        .map(|(i, _)| {
            let raw = args.get(i + 1).ok_or("--ollama-url needs a URL")?;
            normalize_url(raw)
                .ok_or_else(|| format!("--ollama-url {raw}: expected http(s)://host[:port]"))
        })
        .collect()
}

/// Local first, then saved hosts, flag hosts and `OLLAMA_HOST`, each once. Flag hosts are saved.
fn hosts_to_check(saved: &[String], flagged: &[String], env: Option<&str>) -> Vec<String> {
    let env = env.and_then(normalize_url);
    let mut out: Vec<String> = Vec::new();
    for url in std::iter::once(LOCAL_URL.to_string())
        .chain(saved.iter().cloned())
        .chain(flagged.iter().cloned())
        .chain(env)
    {
        if !out.contains(&url) {
            out.push(url);
        }
    }
    out
}

/// `127.0.0.1:11434 3 models (2 chat, 1 embedding)` or `100.99.194.92 unreachable (reason)`.
fn summary(base: &str, found: &Result<(String, Vec<Model>), String>) -> String {
    let label = authority(base);
    let Ok((_, models)) = found else {
        return format!("{label} unreachable ({})", found.as_ref().unwrap_err());
    };
    let chat = models.iter().filter(|m| m.tools && !m.embedding).count();
    let embedding = models.iter().filter(|m| m.embedding).count();
    let other = models.len() - chat - embedding;
    let extra = if other > 0 {
        format!(", {other} without tools")
    } else {
        String::new()
    };
    format!(
        "{label} {} models ({chat} chat, {embedding} embedding{extra})",
        models.len()
    )
}

/// Add the providers to `<root>/opencode.jsonc`. A file that is not plain JSON is left alone and the
/// snippet is returned instead. A changed file is first copied to `opencode.jsonc.bak-a2a-ollama`.
fn write_providers(root: &Path, providers: &[(String, Value)]) -> Result<String, String> {
    if providers.is_empty() {
        return Ok(String::new());
    }
    let path = root.join("opencode.jsonc");
    let mut cfg: Value = if path.exists() {
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => {
                let snippet = json!({ "provider": providers.iter().cloned().collect::<serde_json::Map<_, _>>() });
                return Ok(format!(
                    "{} has comments or trailing commas, left untouched; add this under provider:\n{}",
                    path.display(),
                    serde_json::to_string_pretty(&snippet).unwrap_or_default()
                ));
            }
        }
    } else {
        json!({ "$schema": "https://opencode.ai/config.json" })
    };
    let before = cfg.clone();
    let slot = cfg
        .as_object_mut()
        .ok_or("opencode.jsonc is not a JSON object")?
        .entry("provider")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("`provider` in opencode.jsonc is not an object")?;
    for (id, entry) in providers {
        match slot.get_mut(id) {
            Some(existing) => {
                merge_provider(existing, entry);
            }
            None => {
                slot.insert(id.clone(), entry.clone());
            }
        }
    }
    if cfg == before {
        return Ok(String::new());
    }
    if path.exists() {
        std::fs::copy(&path, root.join("opencode.jsonc.bak-a2a-ollama"))
            .map_err(|e| format!("cannot back up {}: {e}", path.display()))?;
    }
    let out = serde_json::to_string_pretty(&cfg).map_err(|e| e.to_string())?;
    std::fs::write(&path, out + "\n")
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(format!("ollama providers written to {}", path.display()))
}

/// The `init` step: probe the hosts, write the providers, return the lines to print.
/// `--no-ollama` returns nothing. Never fails init: problems become lines.
pub fn setup(root: &Path, args: &[String]) -> Vec<String> {
    setup_with(root, args, std::env::var("OLLAMA_HOST").ok().as_deref())
}

fn setup_with(root: &Path, args: &[String], env: Option<&str>) -> Vec<String> {
    if args.iter().any(|a| a == "--no-ollama") {
        return Vec::new();
    }
    let flagged = match urls_from_args(args) {
        Ok(u) => u,
        Err(e) => return vec![format!("ollama: {e}")],
    };
    let saved = load_hosts(root);
    let mut lines = Vec::new();
    let extra: Vec<String> =
        saved
            .iter()
            .chain(&flagged)
            .cloned()
            .fold(Vec::new(), |mut acc, u| {
                if !acc.contains(&u) {
                    acc.push(u);
                }
                acc
            });
    if extra != saved {
        if let Err(e) = save_hosts(root, &extra) {
            lines.push(format!("ollama: {e}"));
        }
    }
    let mut parts = Vec::new();
    let mut providers = Vec::new();
    let mut embeddings = Vec::new();
    for base in hosts_to_check(&saved, &flagged, env) {
        let found = discover(&base);
        let silent = base == LOCAL_URL && found.is_err() && crate::cli::which("ollama").is_none();
        if !silent {
            parts.push(summary(&base, &found));
        }
        let Ok((effective, models)) = found else {
            continue;
        };
        embeddings.extend(
            models
                .iter()
                .filter(|m| m.embedding)
                .map(|m| format!("{} ({})", m.name, authority(&effective))),
        );
        providers.push((provider_id(&effective), provider_entry(&effective, &models)));
    }
    if !parts.is_empty() {
        lines.push(format!("ollama: {}", parts.join(" | ")));
    }
    if !embeddings.is_empty() {
        lines.push(format!(
            "ollama: embedding models are not added as chat models: {}",
            embeddings.join(", ")
        ));
    }
    match write_providers(root, &providers) {
        Ok(msg) if !msg.is_empty() => lines.push(format!("ollama: {msg}")),
        Ok(_) => {}
        Err(e) => lines.push(format!("ollama: {e}")),
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn tags() -> String {
        json!({ "models": [
            { "name": "qwen3:27b", "size": 17_000_000_000u64 },
            { "name": "embeddinggemma:latest", "size": 621_000_000u64 },
            { "name": "oldchat:7b", "size": 4_000_000_000u64 },
        ] })
        .to_string()
    }

    fn show(name: &str) -> String {
        match name {
            "qwen3:27b" => json!({ "capabilities": ["completion", "tools"], "model_info": { "qwen3.context_length": 40960 } }),
            "embeddinggemma:latest" => json!({ "capabilities": ["embedding"], "model_info": { "gemma3.context_length": 2048 } }),
            _ => json!({ "capabilities": ["completion"] }),
        }
        .to_string()
    }

    /// A fake Ollama on a free port that answers `/api/tags` and `/api/show` until the test ends.
    fn fake_ollama() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { continue };
                let mut buf = vec![0u8; 8192];
                let n = s.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let body = match req.lines().next().unwrap_or("") {
                    l if l.contains("/api/tags") => tags(),
                    l if l.contains("/api/show") => show(
                        req.split("\"model\":\"")
                            .nth(1)
                            .and_then(|r| r.split('"').next())
                            .unwrap_or(""),
                    ),
                    _ => "{}".to_string(),
                };
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        url
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("a2a-ollama-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn urls_are_normalized() {
        assert_eq!(
            normalize_url("100.99.194.92").as_deref(),
            Some("http://100.99.194.92")
        );
        assert_eq!(
            normalize_url("http://h:11434/v1/").as_deref(),
            Some("http://h:11434")
        );
        assert_eq!(normalize_url("0.0.0.0:11434").as_deref(), Some(LOCAL_URL));
        assert_eq!(normalize_url("ftp://h"), None);
        assert_eq!(normalize_url("http://h/some/path"), None);
        assert_eq!(normalize_url("  "), None);
    }

    #[test]
    fn provider_ids_come_from_the_host() {
        assert_eq!(provider_id(LOCAL_URL), "ollama");
        assert_eq!(provider_id("http://100.99.194.92"), "ollama-100-99-194-92");
        assert_eq!(
            provider_id("http://100.99.194.92:11434"),
            "ollama-100-99-194-92"
        );
        assert_eq!(provider_id("http://Box.lan:8080"), "ollama-box-lan-8080");
    }

    #[test]
    fn capabilities_decide_chat_or_embedding() {
        let m = parse_model(
            "qwen3:27b",
            1,
            Some(&serde_json::from_str(&show("qwen3:27b")).unwrap()),
        );
        assert!(m.tools && !m.embedding);
        assert_eq!(m.context, Some(40960));
        let e = parse_model(
            "embeddinggemma:latest",
            1,
            Some(&serde_json::from_str(&show("embeddinggemma:latest")).unwrap()),
        );
        assert!(e.embedding && !e.tools);
        // no /api/show answer: guess from the name
        assert!(parse_model("nomic-embed-text", 1, None).embedding);
        assert!(parse_model("llama3", 1, None).tools);
    }

    #[test]
    fn provider_keeps_only_tool_models_with_limits() {
        let models = vec![
            parse_model(
                "qwen3:27b",
                1,
                Some(&serde_json::from_str(&show("qwen3:27b")).unwrap()),
            ),
            parse_model(
                "embeddinggemma:latest",
                1,
                Some(&serde_json::from_str(&show("embeddinggemma:latest")).unwrap()),
            ),
        ];
        let p = provider_entry("http://h:11434", &models);
        assert_eq!(p["options"]["baseURL"], "http://h:11434/v1");
        assert_eq!(p["npm"], "@ai-sdk/openai-compatible");
        assert!(p["models"]["embeddinggemma:latest"].is_null());
        assert_eq!(
            p["models"]["qwen3:27b"]["limit"],
            json!({ "context": 40960, "output": 8192 })
        );
        assert_eq!(
            p["models"]["qwen3:27b"]["cost"],
            json!({ "input": 0, "output": 0 })
        );
    }

    #[test]
    fn merge_adds_missing_and_keeps_user_edits() {
        let ours = provider_entry(
            "http://h:11434",
            &[
                Model {
                    name: "a".into(),
                    size: 1,
                    context: None,
                    tools: true,
                    embedding: false,
                },
                Model {
                    name: "b".into(),
                    size: 1,
                    context: None,
                    tools: true,
                    embedding: false,
                },
            ],
        );
        let mut theirs = json!({ "name": "My box", "options": { "baseURL": "http://custom/v1" }, "models": { "a": { "name": "Mine" } } });
        assert!(merge_provider(&mut theirs, &ours));
        assert_eq!(theirs["name"], "My box");
        assert_eq!(theirs["options"]["baseURL"], "http://custom/v1");
        assert_eq!(theirs["models"]["a"]["name"], "Mine");
        assert_eq!(theirs["models"]["b"]["name"], "b");
        assert!(
            !merge_provider(&mut theirs, &ours),
            "second merge changes nothing"
        );
    }

    #[test]
    fn discovery_reads_the_fake_server() {
        let url = fake_ollama();
        let (base, models) = discover(&url).unwrap();
        assert_eq!(base, url);
        assert_eq!(models.len(), 3);
        assert!(
            models
                .iter()
                .find(|m| m.name == "embeddinggemma:latest")
                .unwrap()
                .embedding
        );
        assert!(
            !models
                .iter()
                .find(|m| m.name == "oldchat:7b")
                .unwrap()
                .tools
        );
    }

    #[test]
    fn an_unreachable_host_is_an_error_not_a_panic() {
        assert!(discover("http://127.0.0.1:1").is_err());
    }

    #[test]
    fn saved_hosts_round_trip_and_dedupe() {
        let root = tmp("hosts");
        assert!(load_hosts(&root).is_empty());
        save_hosts(&root, &["http://a:1".to_string()]).unwrap();
        assert_eq!(load_hosts(&root), vec!["http://a:1".to_string()]);
        let all = hosts_to_check(
            &["http://a:1".into()],
            &["http://a:1".into(), "http://b".into()],
            Some("b:11434"),
        );
        assert_eq!(
            all,
            vec![
                LOCAL_URL.to_string(),
                "http://a:1".into(),
                "http://b".into(),
                "http://b:11434".into()
            ]
        );
    }

    #[test]
    fn urls_from_args_collects_repeats_and_rejects_bad() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            urls_from_args(&a(&[
                "--ollama-url",
                "h1",
                "--yes",
                "--ollama-url",
                "http://h2:9"
            ]))
            .unwrap(),
            vec!["http://h1", "http://h2:9"]
        );
        assert!(urls_from_args(&a(&["--ollama-url"])).is_err());
        assert!(urls_from_args(&a(&["--ollama-url", "ftp://x"])).is_err());
    }

    #[test]
    fn write_is_idempotent_and_backs_up() {
        let root = tmp("write");
        let p = vec![(
            "ollama-x".to_string(),
            provider_entry(
                "http://x:1",
                &[Model {
                    name: "a".into(),
                    size: 1,
                    context: None,
                    tools: true,
                    embedding: false,
                }],
            ),
        )];
        std::fs::write(
            root.join("opencode.jsonc"),
            r#"{"default_agent":"orchestrator"}"#,
        )
        .unwrap();
        assert!(write_providers(&root, &p).unwrap().contains("written"));
        assert!(root.join("opencode.jsonc.bak-a2a-ollama").exists());
        let once = std::fs::read_to_string(root.join("opencode.jsonc")).unwrap();
        assert!(once.contains("default_agent") && once.contains("ollama-x"));
        assert_eq!(write_providers(&root, &p).unwrap(), "");
        assert_eq!(
            std::fs::read_to_string(root.join("opencode.jsonc")).unwrap(),
            once
        );
    }

    #[test]
    fn a_file_with_comments_is_not_rewritten() {
        let root = tmp("comments");
        let text = "{\n // mine\n \"a\": 1,\n}\n";
        std::fs::write(root.join("opencode.jsonc"), text).unwrap();
        let p = vec![("ollama".to_string(), json!({}))];
        assert!(
            write_providers(&root, &p)
                .unwrap()
                .contains("left untouched")
        );
        assert_eq!(
            std::fs::read_to_string(root.join("opencode.jsonc")).unwrap(),
            text
        );
    }

    #[test]
    fn no_ollama_does_nothing_and_a_dead_host_is_one_line() {
        let root = tmp("setup");
        assert!(setup_with(&root, &["--no-ollama".to_string()], None).is_empty());
        assert!(!root.join("opencode.jsonc").exists());
        let lines = setup_with(
            &root,
            &["--ollama-url".to_string(), "http://127.0.0.1:1".to_string()],
            None,
        );
        assert!(
            lines.iter().any(|l| l.contains("127.0.0.1:1 unreachable")),
            "{lines:?}"
        );
    }
}
