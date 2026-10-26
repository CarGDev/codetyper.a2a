//! Authenticated task assignment: with a trusted-keys file present, only the orchestrator holding
//! a valid signed credential may create tasks; spoofed `from`, missing/replayed/foreign
//! credentials and the `gentle-orchestrator` target are denied and leave no rows.
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_codetypera2a");

struct Env(PathBuf);

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

impl Env {
    fn new(name: &str) -> Env {
        let dir = std::env::temp_dir().join(format!("a2a-guard-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        std::fs::create_dir_all(dir.join("home/auth")).unwrap();
        let env = Env(dir);
        env.call(None, "list_agents", json!({})).unwrap();
        env
    }

    fn project(&self) -> String {
        self.0.join("proj").display().to_string()
    }

    fn trust(&self, key: &SigningKey) {
        let line = format!("k1 {}", hex(key.verifying_key().as_bytes()));
        std::fs::write(self.0.join("home/auth/trusted-keys"), line).unwrap();
    }

    fn live_session(&self, session: &str) {
        let conn = rusqlite::Connection::open(self.0.join("hub.db")).unwrap();
        conn.execute("INSERT OR IGNORE INTO agents (name, card, status, created_at, updated_at) VALUES ('orchestrator','{}','active','t','t')", []).unwrap();
        conn.execute("INSERT INTO sessions (agent, directory, session_id, status, created_at) VALUES ('orchestrator', ?, ?, 'live', 't')", [&self.project(), session]).unwrap();
    }

    fn credential(&self, key: &SigningKey, edit: impl Fn(&mut Value)) -> String {
        let mut c = json!({"v":1,"iss":"codetypera2a-runtime","aud":"codetyper-hub","sub":"s1",
            "agent":"orchestrator","project":self.project(),"iat":now(),"exp":now()+600,
            "jti":format!("jti-{}", uuid()),"kid":"k1"});
        edit(&mut c);
        let payload = c.to_string();
        format!(
            "a2a1.{}.{}",
            hex(payload.as_bytes()),
            hex(&key.sign(payload.as_bytes()).to_bytes())
        )
    }

    /// One tool call against a fresh hub; `credential` is written to a one-shot 0600 file.
    fn call(&self, credential: Option<&str>, tool: &str, args: Value) -> Result<Value, String> {
        let mut cmd = Command::new(BIN);
        cmd.arg("hub")
            .env("A2A_DB", self.0.join("hub.db"))
            .env("CODETYPERA2A_HOME", self.0.join("home"))
            .env("A2A_AGENT_DIR", self.0.join("agents"))
            .env("A2A_NO_DISPATCHER", "1")
            .env("OPENCODE_BIN", "/usr/bin/false")
            .env_remove("TMUX")
            .env_remove("TMUX_PANE");
        if let Some(text) = credential {
            use std::os::unix::fs::PermissionsExt;
            let path = self.0.join(format!("cred-{}", uuid()));
            std::fs::write(&path, text).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            cmd.env("A2A_CREDENTIAL_FILE", path);
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let req = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":tool,"arguments":args}});
        writeln!(child.stdin.take().unwrap(), "{req}").unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let _ = child.wait();
        let v: Value = serde_json::from_str(&line).unwrap();
        let result = &v["result"];
        if result["isError"] == json!(true) {
            return Err(result["content"][0]["text"].as_str().unwrap().to_string());
        }
        Ok(result["structuredContent"].clone())
    }

    fn send(&self, cred: Option<&str>, from: &str, to: &str, dir: &str) -> Result<Value, String> {
        self.call(
            cred,
            "send_message",
            json!({"from":from,"to":to,"text":"x","fromDirectory":dir,"directory":dir}),
        )
    }

    fn task_count(&self) -> i64 {
        let conn = rusqlite::Connection::open(self.0.join("hub.db")).unwrap();
        conn.query_row("SELECT count(*) FROM tasks", [], |r| r.get(0))
            .unwrap()
    }
}

fn uuid() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    )
}

fn key() -> SigningKey {
    SigningKey::from_bytes(&[5u8; 32])
}

#[test]
fn the_authenticated_primary_creates_tasks_and_everything_else_is_denied() {
    let env = Env::new("create");
    env.trust(&key());
    env.live_session("s1");
    let dir = env.project();
    let ok = env.credential(&key(), |_| {});
    let t = env.send(Some(&ok), "orchestrator", "build", &dir).unwrap();
    assert_eq!(t["from"], "orchestrator");
    let any = env.credential(&key(), |_| {});
    env.send(Some(&any), "orchestrator", "any", &dir).unwrap();
    assert_eq!(env.task_count(), 2);

    let denied = |cred: Option<String>, from: &str, to: &str, dir: &str| {
        let err = env.send(cred.as_deref(), from, to, dir).unwrap_err();
        assert!(err.contains("denied"), "{err}");
        assert!(!err.contains("s1") && !err.contains("jti"), "{err}");
    };
    denied(None, "orchestrator", "build", &dir); // no credential: spoofed from
    denied(Some(env.credential(&key(), |_| {})), "build", "plan", &dir); // mismatched from
    denied(
        Some(env.credential(&key(), |c| c["agent"] = "build".into())),
        "build",
        "plan",
        &dir,
    );
    denied(
        Some(env.credential(&key(), |c| c["project"] = "/elsewhere".into())),
        "orchestrator",
        "build",
        &dir,
    );
    denied(
        Some(env.credential(&key(), |c| c["sub"] = "ghost".into())),
        "orchestrator",
        "build",
        &dir,
    );
    denied(
        Some(env.credential(&SigningKey::from_bytes(&[6u8; 32]), |_| {})),
        "orchestrator",
        "build",
        &dir,
    );
    denied(
        Some(env.credential(&key(), |c| c["agent"] = "gentle-orchestrator".into())),
        "gentle-orchestrator",
        "build",
        &dir,
    );
    denied(
        Some(env.credential(&key(), |_| {})),
        "orchestrator",
        "gentle-orchestrator",
        &dir,
    );
    denied(
        Some(env.credential(&key(), |_| {})),
        "orchestrator",
        "build",
        "/elsewhere",
    ); // cross-project request
    assert_eq!(env.task_count(), 2, "denied calls must create no rows");
}

#[test]
fn a_consumed_credential_cannot_be_replayed() {
    let env = Env::new("replay");
    env.trust(&key());
    env.live_session("s1");
    let dir = env.project();
    let cred = env.credential(&key(), |_| {});
    env.send(Some(&cred), "orchestrator", "build", &dir)
        .unwrap();
    // the hub unlinked the one-shot file; a new connection pointing at the gone file has no identity
    let err = env.call(None, "send_message", json!({"from":"orchestrator","to":"build","text":"x","fromDirectory":dir,"directory":dir})).unwrap_err();
    assert!(err.contains("denied"), "{err}");
}

#[test]
fn assignees_still_work_but_cannot_act_as_the_requester() {
    let env = Env::new("lifecycle");
    env.trust(&key());
    env.live_session("s1");
    let dir = env.project();
    let t = env
        .send(
            Some(&env.credential(&key(), |_| {})),
            "orchestrator",
            "build",
            &dir,
        )
        .unwrap();
    let id = t["id"].as_str().unwrap();
    // the assignee claims/updates with no credential (existing ownership rules)
    env.call(None, "claim_task", json!({"id": id, "agent": "build"}))
        .unwrap();
    env.call(
        None,
        "update_task",
        json!({"id": id, "agent": "build", "state": "completed", "text": "done"}),
    )
    .unwrap();
    // a delegated process cannot reply or cancel as the orchestrator
    let t2 = env
        .send(
            Some(&env.credential(&key(), |_| {})),
            "orchestrator",
            "build",
            &dir,
        )
        .unwrap();
    let id2 = t2["id"].as_str().unwrap();
    let err = env
        .call(
            None,
            "send_message",
            json!({"from":"orchestrator","taskId":id2,"text":"hi"}),
        )
        .unwrap_err();
    assert!(err.contains("denied"), "{err}");
    let err = env
        .call(
            None,
            "cancel_task",
            json!({"id": id2, "agent": "orchestrator"}),
        )
        .unwrap_err();
    assert!(err.contains("denied"), "{err}");
    // the authenticated orchestrator can
    let good = env.credential(&key(), |_| {});
    env.call(
        Some(&good),
        "send_message",
        json!({"from":"orchestrator","taskId":id2,"text":"hi"}),
    )
    .unwrap();
}

#[test]
fn without_a_trusted_keys_file_behavior_is_unchanged() {
    let env = Env::new("legacy");
    let dir = env.project();
    env.send(None, "orchestrator", "build", &dir).unwrap();
    assert_eq!(env.task_count(), 1);
}

#[test]
fn the_harness_orchestrator_is_neither_listed_nor_a_valid_target() {
    let env = Env::new("forbidden");
    std::fs::write(
        env.0.join("agents/gentle-orchestrator.md"),
        "---\ndescription: x\nmode: primary\n---\nb\n",
    )
    .unwrap();
    let listed = env
        .call(None, "list_agents", json!({"query": "orchestrator"}))
        .unwrap();
    assert!(
        !listed.to_string().contains("gentle-orchestrator"),
        "{listed}"
    );
    let err = env
        .send(
            None,
            "security-scanner",
            "gentle-orchestrator",
            &env.project(),
        )
        .unwrap_err();
    assert!(err.contains("denied"), "{err}");
    assert_eq!(env.task_count(), 0);
}
