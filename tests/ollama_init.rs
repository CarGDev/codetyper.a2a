//! `init --opencode --ollama-url` against a fake Ollama: the provider and its tool-capable models land in
//! opencode.jsonc, embedding models are only listed, a rerun changes nothing. Temp dirs only, no real host.
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_codetypera2a");

fn fake_ollama() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let mut buf = vec![0u8; 8192];
            let n = s.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let body = if req.starts_with("GET /api/tags") {
                r#"{"models":[{"name":"qwen3:27b","size":1},{"name":"embeddinggemma:latest","size":1}]}"#
            } else if req.contains("embeddinggemma") {
                r#"{"capabilities":["embedding"],"model_info":{"gemma3.context_length":2048}}"#
            } else {
                r#"{"capabilities":["completion","tools"],"model_info":{"qwen3.context_length":40960}}"#
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

fn sandbox(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("a2a-ollama-init-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("home")).unwrap();
    d
}

fn init(root: &Path, extra: &[&str]) -> String {
    let out = Command::new(BIN)
        .args(["init", "--opencode", "--yes", "--no-models", "--no-link"])
        .args(extra)
        .env_clear()
        .env("HOME", root.join("home"))
        .env("PATH", "/usr/bin:/bin")
        .env("OPENCODE_CONFIG_DIR", root.join("oc"))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn config(root: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(root.join("oc/opencode.jsonc")).unwrap()).unwrap()
}

#[test]
fn adds_the_provider_once_and_remembers_the_host() {
    let root = sandbox("add");
    let url = fake_ollama();
    let out = init(&root, &["--ollama-url", &url]);
    assert!(out.contains("2 models (1 chat, 1 embedding)"), "{out}");
    assert!(
        out.contains("embedding models are not added as chat models: embeddinggemma:latest"),
        "{out}"
    );
    let id = format!("ollama-127-0-0-1-{}", url.rsplit(':').next().unwrap());
    let p = &config(&root)["provider"][&id];
    assert_eq!(p["options"]["baseURL"], format!("{url}/v1"));
    assert!(p["models"]["qwen3:27b"].is_object() && p["models"]["embeddinggemma:latest"].is_null());
    assert_eq!(p["models"]["qwen3:27b"]["limit"]["context"], 40960);

    // second run, no flag: the saved host is re-checked and nothing is rewritten
    let before = std::fs::read_to_string(root.join("oc/opencode.jsonc")).unwrap();
    let out = init(&root, &[]);
    assert!(out.contains("2 models"), "saved host not re-checked: {out}");
    assert!(!out.contains("providers written"), "{out}");
    assert_eq!(
        std::fs::read_to_string(root.join("oc/opencode.jsonc")).unwrap(),
        before
    );
}

#[test]
fn an_unreachable_host_is_one_line_and_init_succeeds() {
    let root = sandbox("dead");
    let out = init(&root, &["--ollama-url", "http://127.0.0.1:1"]);
    assert_eq!(
        out.lines()
            .filter(|l| l.contains("127.0.0.1:1 unreachable"))
            .count(),
        1,
        "{out}"
    );
    assert!(config(&root).get("mcp").is_some(), "init did not finish");
}

#[test]
fn no_ollama_skips_everything() {
    let root = sandbox("skip");
    let url = fake_ollama();
    let out = init(&root, &["--ollama-url", &url, "--no-ollama"]);
    assert!(!out.contains("init: ollama:"), "{out}");
    assert!(config(&root).get("provider").is_none());
    assert!(!root.join("oc/a2a/ollama.json").exists());
}
