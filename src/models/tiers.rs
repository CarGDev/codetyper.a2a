/// Preference lists, best first; the first one opencode has wins. Model ids, without the provider.
const CHEAP: &[&str] = &[
    "claude-haiku-5.5",
    "gpt-6-luna",
    "gpt-5.6-luna",
    "claude-haiku-4.5",
    "gpt-5.4-nano",
    "mai-code-1.1-flash",
];
const BALANCED: &[&str] = &[
    "claude-sonnet-5.5",
    "claude-sonnet-5",
    "gpt-5.6-terra",
    "claude-sonnet-4.6",
];
const DEEP: &[&str] = &[
    "claude-opus-5.5",
    "gpt-6.1-sol",
    "claude-opus-5",
    "claude-sonnet-5.5",
    "claude-sonnet-5",
];
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tier {
    Cheap,
    Balanced,
    Deep,
}

impl Tier {
    fn list(self) -> &'static [&'static str] {
        match self {
            Tier::Cheap => CHEAP,
            Tier::Balanced => BALANCED,
            Tier::Deep => DEEP,
        }
    }

    pub fn why(self) -> &'static str {
        match self {
            Tier::Cheap => {
                "light work (routing, running commands, summaries, lookups): fast and cheap"
            }
            Tier::Balanced => "everyday coding and agentic work: capable without the top price",
            Tier::Deep => "design and long-horizon reasoning: worth the strongest model",
        }
    }
}

/// Agents that only route, run commands, summarise or look things up.
const CHEAP_AGENTS: &[&str] = &[
    "summarizer",
    "doc-lookup",
    "git-historian",
    "test-runner",
    "pr-commenter",
    "brain",
];

/// Every other shipped agent. Deep is never a default: it is opt-in.
const BALANCED_AGENTS: &[&str] = &[
    "orchestrator",
    "plan",
    "build",
    "architect",
    "mentor",
    "code-debugger",
    "code-reviewer",
    "pr-reviewer",
    "security-scanner",
    "owasp-guardian",
    "test-case-generator",
    "react-frontend-expert",
    "typescript-mcp-expert",
    "api-design-architect",
    "software-architecture-expert",
    "playwright-tester",
];

/// The default tier of a shipped agent; `None` for any other name.
pub fn tier_for(agent: &str) -> Option<Tier> {
    [
        (CHEAP_AGENTS, Tier::Cheap),
        (BALANCED_AGENTS, Tier::Balanced),
    ]
    .into_iter()
    .find(|(names, _)| names.contains(&agent))
    .map(|(_, tier)| tier)
}

/// Deep for an agent the user put on the deep list, else its default tier.
pub fn effective_tier(agent: &str, deep: &super::deep::DeepList) -> Option<Tier> {
    if deep.contains(agent) {
        return Some(Tier::Deep);
    }
    tier_for(agent)
}

/// The Claude Code model name of a tier.
pub fn claude_alias(tier: Tier) -> &'static str {
    match tier {
        Tier::Cheap => "haiku",
        Tier::Balanced => "sonnet",
        Tier::Deep => "opus",
    }
}

/// `provider/model#variant` -> `model` (what the preference lists use).
fn model_id(full: &str) -> &str {
    let base = full.split('#').next().unwrap_or(full);
    base.split_once('/').map(|(_, m)| m).unwrap_or(base)
}

/// The best model of `tier` that `available` (`provider/model` strings) contains; GitHub Copilot first.
pub fn recommend(tier: Tier, available: &[String]) -> Option<String> {
    for id in tier.list() {
        let hits: Vec<&String> = available.iter().filter(|m| model_id(m) == *id).collect();
        if let Some(m) = hits
            .iter()
            .find(|m| m.starts_with(COPILOT_PREFIX))
            .or_else(|| hits.first())
        {
            return Some((*m).clone());
        }
    }
    price_fallback(tier, available)
}

const COPILOT_PREFIX: &str = "github-copilot/";

/// When no preferred model is installed: pick among the installed models that have a known price.
/// Cheap is the cheapest, Balanced the lower median, Deep the most expensive. Equal prices go to
/// GitHub Copilot first, then to the name, so the result is stable and always one of `available`.
fn price_fallback(tier: Tier, available: &[String]) -> Option<String> {
    let mut priced: Vec<(f64, &String)> = available
        .iter()
        .filter_map(|m| price(m).map(|(input, output)| (input + output, m)))
        .collect();
    priced.sort_by(|(a_cost, a), (b_cost, b)| {
        a_cost
            .total_cmp(b_cost)
            .then_with(|| {
                b.starts_with(COPILOT_PREFIX)
                    .cmp(&a.starts_with(COPILOT_PREFIX))
            })
            .then_with(|| a.cmp(b))
    });
    let highest = priced.last()?.0;
    let index = match tier {
        Tier::Cheap => 0,
        Tier::Balanced => (priced.len() - 1) / 2,
        Tier::Deep => priced.iter().position(|(cost, _)| *cost == highest)?,
    };
    priced.get(index).map(|(_, model)| (*model).clone())
}

/// Published GitHub Copilot token prices, USD per 1M tokens input/output (docs.github.com, 2026-10).
pub fn price_hint(model: &str) -> &'static str {
    match model_id(model) {
        "claude-haiku-5.5" | "gpt-6-luna" => "$0.10 / $0.50",
        "gpt-5.6-luna" | "mai-code-1.1-flash" => "$0.20 / $1.20",
        "gpt-5.4-nano" => "$0.20 / $1.25",
        "gemini-3.8-flash" => "$0.75 / $3.75",
        "claude-haiku-4.5" => "$1 / $5",
        "gpt-5.3-codex" => "$1.75 / $14",
        "claude-sonnet-5" | "claude-sonnet-5.5" | "gpt-6-sol" | "gpt-6.1-sol"
        | "gemini-4-argon" => "$2 / $10",
        "gpt-5.6-terra" => "$2 / $12",
        "grok-4.5" | "grok-4.6" | "grok-4.7" => "$2 / $6",
        "claude-sonnet-4.6" | "kimi-k3" => "$3 / $15",
        "claude-opus-5.5" | "gpt-5.6-sol" => "$4 / $20",
        "claude-opus-4.8" | "claude-opus-5" => "$5 / $25",
        "claude-fable-5" | "claude-fable-5.1" | "gpt-6-astra" => "$10 / $50",
        _ => "",
    }
}

/// `price_hint` as numbers: USD per 1M tokens, (input, output). None for unpriced models.
pub fn price(model: &str) -> Option<(f64, f64)> {
    let (i, o) = price_hint(model).split_once('/')?;
    let n = |t: &str| t.trim().trim_start_matches('$').parse::<f64>().ok();
    Some((n(i)?, n(o)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::config::frontmatter_model;
    use std::path::Path;
    fn avail(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn recommends_by_role_and_falls_back() {
        let all = avail(&[
            "github-copilot/claude-haiku-5.5",
            "github-copilot/claude-sonnet-5.5",
            "github-copilot/claude-opus-5.5",
            "github-copilot/gpt-6.1-sol",
        ]);
        assert_eq!(
            recommend(Tier::Cheap, &all).as_deref(),
            Some("github-copilot/claude-haiku-5.5")
        );
        assert_eq!(
            recommend(Tier::Balanced, &all).as_deref(),
            Some("github-copilot/claude-sonnet-5.5")
        );
        assert_eq!(
            recommend(Tier::Deep, &all).as_deref(),
            Some("github-copilot/claude-opus-5.5")
        );
        // nothing suitable: no recommendation
        assert_eq!(recommend(Tier::Deep, &avail(&["x/unknown"])), None);
        // a direct provider works too, and GitHub Copilot is preferred when both exist
        let both = avail(&[
            "anthropic/claude-sonnet-5.5",
            "github-copilot/claude-sonnet-5.5",
        ]);
        assert_eq!(
            recommend(Tier::Balanced, &both).as_deref(),
            Some("github-copilot/claude-sonnet-5.5")
        );
        assert_eq!(
            recommend(Tier::Balanced, &avail(&["anthropic/claude-sonnet-5.5"])).as_deref(),
            Some("anthropic/claude-sonnet-5.5")
        );
    }

    const CHEAP_AGENTS: [&str; 6] = [
        "summarizer",
        "doc-lookup",
        "git-historian",
        "test-runner",
        "pr-commenter",
        "brain",
    ];

    #[test]
    fn the_tier_table_is_fixed_for_every_agent() {
        let names: Vec<String> = templates().into_iter().map(|(name, _)| name).collect();
        assert_eq!(names.len(), 22);
        for name in &names {
            let want = if CHEAP_AGENTS.contains(&name.as_str()) {
                Tier::Cheap
            } else {
                Tier::Balanced
            };
            assert_eq!(tier_for(name), Some(want), "{name}");
        }
        assert_eq!(tier_for("someones-own-agent"), None);
        assert_eq!(tier_for(""), None);
        assert_eq!(tier_for("__proto__"), None);
    }

    #[test]
    fn the_agent_tables_are_disjoint() {
        for name in CHEAP_AGENTS {
            assert!(!BALANCED_AGENTS.contains(&name), "{name} is in both tables");
        }
    }

    #[test]
    fn the_price_fallback_picks_by_tier_among_installed_priced_models() {
        let none_preferred = avail(&[
            "x/unpriced",
            "github-copilot/gemini-3.8-flash",
            "github-copilot/claude-haiku-4.5",
            "github-copilot/grok-4.5",
            "github-copilot/claude-sonnet-4.5-unknown",
            "github-copilot/claude-fable-5",
        ]);
        assert_eq!(
            price_fallback(Tier::Cheap, &none_preferred).as_deref(),
            Some("github-copilot/gemini-3.8-flash")
        );
        assert_eq!(
            price_fallback(Tier::Balanced, &none_preferred).as_deref(),
            Some("github-copilot/claude-haiku-4.5")
        );
        assert_eq!(
            price_fallback(Tier::Deep, &none_preferred).as_deref(),
            Some("github-copilot/claude-fable-5")
        );
        assert_eq!(price_fallback(Tier::Cheap, &avail(&["x/unpriced"])), None);
        assert_eq!(price_fallback(Tier::Deep, &[]), None);
    }

    #[test]
    fn price_ties_prefer_github_copilot_then_the_name() {
        let tied = avail(&["z/grok-4.6", "a/grok-4.7", "github-copilot/grok-4.5"]);
        assert_eq!(
            price_fallback(Tier::Cheap, &tied).as_deref(),
            Some("github-copilot/grok-4.5")
        );
        let tied = avail(&["z/grok-4.6", "a/grok-4.7"]);
        assert_eq!(
            price_fallback(Tier::Cheap, &tied).as_deref(),
            Some("a/grok-4.7")
        );
    }

    #[test]
    fn recommend_prefers_the_lists_and_only_then_falls_back_by_price() {
        let listed = avail(&["github-copilot/claude-sonnet-5.5", "x/grok-4.5"]);
        assert_eq!(
            recommend(Tier::Balanced, &listed).as_deref(),
            Some("github-copilot/claude-sonnet-5.5")
        );
        let unlisted = avail(&["x/grok-4.5", "x/kimi-k3"]);
        let got = recommend(Tier::Balanced, &unlisted).unwrap();
        assert!(unlisted.contains(&got), "{got} is not installed");
        assert_eq!(recommend(Tier::Cheap, &avail(&["x/unpriced"])), None);
    }

    #[test]
    fn claude_aliases_follow_the_tiers() {
        assert_eq!(claude_alias(Tier::Cheap), "haiku");
        assert_eq!(claude_alias(Tier::Balanced), "sonnet");
        assert_eq!(claude_alias(Tier::Deep), "opus");
    }

    fn templates() -> Vec<(String, Option<String>)> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("agents");
        let mut v: Vec<(String, Option<String>)> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| {
                (
                    e.path().file_stem().unwrap().to_string_lossy().into_owned(),
                    frontmatter_model(&e.path()),
                )
            })
            .collect();
        v.sort();
        v
    }

    #[test]
    fn every_template_carries_the_recommended_model_of_its_tier() {
        let t = templates();
        assert!(
            t.len() >= 22,
            "expected all agent templates, saw {}",
            t.len()
        );
        for (name, model) in t {
            let tier = tier_for(&name).unwrap_or_else(|| panic!("{name}: no tier in tier_for()"));
            let want = format!("github-copilot/{}", tier.list()[0]);
            assert_eq!(
                model.as_deref(),
                Some(want.as_str()),
                "{name}: template model must be its tier's first choice"
            );
        }
    }

    #[test]
    fn the_shipped_policy_allows_every_template_model_and_keeps_readonly_agents_cheap() {
        let policy = crate::model_guard::load_policy(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("model-policy.json"),
        )
        .unwrap();
        let t = templates();
        for (name, model) in &t {
            let m = model.as_deref().unwrap();
            let c = crate::model_guard::check(m, Some(&policy));
            assert!(
                c.ok,
                "{name}: {m} must be allowed by the shipped policy: {:?}",
                c.reason
            );
            assert_eq!(
                policy.agent_defaults.get(name).map(String::as_str),
                Some(m),
                "{name}: agentDefaults must match the template"
            );
        }
        for name in &policy.read_only_agents {
            if let Some((_, Some(m))) = t.iter().find(|(n, _)| n == name) {
                assert_eq!(
                    crate::model_guard::check(m, Some(&policy)).tier.as_deref(),
                    Some("cheap"),
                    "{name} is listed read-only, so it must use a cheap model"
                );
            }
        }
    }
}
