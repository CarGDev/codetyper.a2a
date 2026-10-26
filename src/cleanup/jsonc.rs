//! JSON-with-comments reading and the key removals `uninstall` performs on config files.
//! Comments and trailing commas are not kept when a file is rewritten.
use serde_json::Value;

/// Comments (`//`, `/* */`) and trailing commas removed; string contents are never touched.
pub fn strip(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let (mut i, mut in_str) = (0, false);
    while i < chars.len() {
        let c = chars[i];
        if in_str {
            out.push(c);
            if c == '\\' && i + 1 < chars.len() {
                i += 1;
                out.push(chars[i]);
            } else if c == '"' {
                in_str = false;
            }
        } else if c == '"' {
            in_str = true;
            out.push(c);
        } else if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        } else if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i += 2;
            continue;
        } else {
            out.push(c);
        }
        i += 1;
    }
    drop_trailing_commas(&out)
}

fn drop_trailing_commas(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let (mut in_str, mut i) = (false, 0);
    while i < chars.len() {
        let c = chars[i];
        if in_str {
            out.push(c);
            if c == '\\' && i + 1 < chars.len() {
                i += 1;
                out.push(chars[i]);
            } else if c == '"' {
                in_str = false;
            }
        } else if c == '"' {
            in_str = true;
            out.push(c);
        } else if c == ',' && next_is_close(&chars, i + 1) {
            // skip the comma
        } else {
            out.push(c);
        }
        i += 1;
    }
    out
}

fn next_is_close(chars: &[char], from: usize) -> bool {
    chars[from.min(chars.len())..]
        .iter()
        .find(|c| !c.is_whitespace())
        .is_some_and(|c| *c == '}' || *c == ']')
}

pub fn parse(text: &str) -> Result<Value, String> {
    serde_json::from_str(&strip(text)).map_err(|e| e.to_string())
}

const COMPANIONS: [&str; 4] = ["codegraph", "context7", "engram", "playwright"];
const OUR_SERVER: &str = "a2a";
const OUR_AGENT: &str = "orchestrator";

fn ours(command: &str) -> bool {
    command.contains("codetypera2a") && command.contains("statusline")
}

/// Remove `a2a` and the companion servers from a map of MCP servers. Returns what was removed.
fn drop_servers(servers: Option<&mut Value>, removed: &mut Vec<String>) {
    let Some(map) = servers.and_then(Value::as_object_mut) else {
        return;
    };
    for name in std::iter::once(OUR_SERVER).chain(COMPANIONS) {
        if map.remove(name).is_some() {
            removed.push(format!("mcp server {name}"));
        }
    }
}

/// `opencode.jsonc`: our server and the companions, `default_agent`, the `ollama*` providers.
pub fn clean_opencode(root: &mut Value) -> Vec<String> {
    let mut removed = Vec::new();
    drop_servers(root.pointer_mut("/mcp/servers"), &mut removed);
    if root["default_agent"] == OUR_AGENT {
        root.as_object_mut().map(|o| o.remove("default_agent"));
        removed.push("default_agent".into());
    }
    if let Some(providers) = root.get_mut("provider").and_then(Value::as_object_mut) {
        let names: Vec<String> = providers
            .keys()
            .filter(|k| *k == "ollama" || k.starts_with("ollama-"))
            .cloned()
            .collect();
        for name in names {
            providers.remove(&name);
            removed.push(format!("provider {name}"));
        }
    }
    removed
}

/// `~/.claude.json`: our server and the companions.
pub fn clean_claude_json(root: &mut Value) -> Vec<String> {
    let mut removed = Vec::new();
    drop_servers(root.get_mut("mcpServers"), &mut removed);
    removed
}

fn drop_group_hooks(root: &mut Value, removed: &mut Vec<String>) {
    let Some(events) = root.get_mut("hooks").and_then(Value::as_object_mut) else {
        return;
    };
    for (event, groups) in events.iter_mut() {
        let Some(list) = groups.as_array_mut() else {
            continue;
        };
        for group in list.iter_mut() {
            let Some(hooks) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
                continue;
            };
            let before = hooks.len();
            hooks.retain(|h| !ours(h["command"].as_str().unwrap_or("")));
            if hooks.len() != before {
                removed.push(format!("hook {event}"));
            }
        }
        list.retain(|g| g["hooks"].as_array().is_none_or(|h| !h.is_empty()));
    }
    events.retain(|_, v| v.as_array().is_none_or(|a| !a.is_empty()));
}

/// Claude Code `settings.json`: our status line and hooks, the default agent, the pane plugin entries.
pub fn clean_claude_settings(root: &mut Value) -> Vec<String> {
    let mut removed = Vec::new();
    if ours(root["statusLine"]["command"].as_str().unwrap_or("")) {
        root.as_object_mut().map(|o| o.remove("statusLine"));
        removed.push("statusLine".into());
    }
    drop_group_hooks(root, &mut removed);
    if root["agent"] == OUR_AGENT {
        root.as_object_mut().map(|o| o.remove("agent"));
        removed.push("agent".into());
    }
    for group in ["enabledPlugins", "extraKnownMarketplaces", "pluginConfigs"] {
        let Some(map) = root.get_mut(group).and_then(Value::as_object_mut) else {
            continue;
        };
        let names: Vec<String> = map
            .keys()
            .filter(|k| k.as_str() == "codetypera2a" || k.ends_with("@codetypera2a"))
            .cloned()
            .collect();
        for name in names {
            map.remove(&name);
            removed.push(format!("{group} {name}"));
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn comments_and_trailing_commas_go_but_strings_are_kept() {
        let v = parse("// c\n{ \"u\": \"http://x/*y*/\", /* b */ \"a\": [1,2,], }").unwrap();
        assert_eq!(v, json!({"u": "http://x/*y*/", "a": [1, 2]}));
    }

    #[test]
    fn opencode_loses_only_our_entries() {
        let mut v = json!({"model":"m","default_agent":"orchestrator","mcp":{"servers":{"a2a":{},"engram":{},"mine":{}}},
            "provider":{"ollama-1-2":{},"other":{}}});
        let removed = clean_opencode(&mut v);
        assert_eq!(removed.len(), 4, "{removed:?}");
        assert_eq!(
            v,
            json!({"model":"m","mcp":{"servers":{"mine":{}}},"provider":{"other":{}}})
        );
        let mut keep = json!({"default_agent":"mine"});
        assert!(clean_opencode(&mut keep).is_empty());
    }

    #[test]
    fn claude_settings_keep_other_tools_hooks() {
        let mut v = json!({"agent":"orchestrator",
            "statusLine":{"command":"'/x/codetypera2a' oc statusline"},
            "enabledPlugins":{"a2a-pane@codetypera2a":true,"other@x":true},
            "hooks":{"SessionStart":[{"hooks":[{"command":"'/x/codetypera2a' oc statusline --hook ensure"},{"command":"gentle-ai x"}]}],
                     "SubagentStart":[{"hooks":[{"command":"codetypera2a oc statusline --hook start"}]}]}});
        clean_claude_settings(&mut v);
        assert_eq!(
            v,
            json!({"enabledPlugins":{"other@x":true},
            "hooks":{"SessionStart":[{"hooks":[{"command":"gentle-ai x"}]}]}})
        );
    }

    #[test]
    fn claude_json_loses_a2a_and_companions() {
        let mut v = json!({"mcpServers":{"a2a":{},"context7":{},"mine":{}}});
        assert_eq!(clean_claude_json(&mut v).len(), 2);
        assert_eq!(v, json!({"mcpServers":{"mine":{}}}));
    }
}
