//! The allowed and forbidden actions of agents (settings `actions.allow` and `actions.forbid`): one list each, of
//! patterns with a reason. A pattern is `<kind>:<name>`, with `*` for any text in the name:
//!
//! - `op:<operation>`: allowed runs with no toolset question; forbidden is refused at dispatch.
//! - `toolset:<set>`: allowed may join a grant (a shipped, system or user set; a project's own set needs your answer
//!   instead); forbidden never does.
//! - `tool:<server>/<tool>`: a tool of a tool server that legionctl brokers; a server with allowed entries offers only
//!   those, forbidden ones never.
//! - `git:<subcommand> <flags>`: forbidden git commands in a session sandbox (the git gate), `push`, `reset --hard`.
//! - `cmd:<command>`: forbidden shell commands (Claude Code's `permissions.deny` of a session).
//!
//! Forbidden wins over allowed. The lists bind agents; your own calls are yours.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The kinds of action that a pattern can name.
pub const KINDS: [&str; 5] = ["op", "toolset", "tool", "git", "cmd"];

/// One entry of a list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// `<kind>:<name>`; `*` in the name is any text.
    pub pattern: String,
    /// Why, in a few words, for the person who reads the list.
    pub reason: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ActionsConfig {
    #[schemars(extend("x-rules" = true))]
    pub allow: Vec<Rule>,
    #[schemars(extend("x-rules" = true))]
    pub forbid: Vec<Rule>,
}

/// Whether `text` matches `glob` (`*` is any text, also none).
fn glob(glob: &str, text: &str) -> bool {
    let mut parts = glob.split('*');
    let first = parts.next().unwrap_or_default();
    let Some(mut rest) = text.strip_prefix(first) else {
        return false;
    };
    let parts: Vec<&str> = parts.collect();
    let Some((last, middle)) = parts.split_last() else {
        return rest.is_empty();
    };
    for part in middle {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    rest.ends_with(last)
}

impl Rule {
    /// The kind and the name of the pattern.
    #[must_use]
    pub fn parts(&self) -> Option<(&str, &str)> {
        self.pattern
            .split_once(':')
            .filter(|(kind, name)| KINDS.contains(kind) && !name.is_empty())
    }

    fn matches(&self, kind: &str, name: &str) -> bool {
        self.parts()
            .is_some_and(|(k, pattern)| k == kind && glob(pattern, name))
    }
}

/// Why `rule` is not a usable entry, if it is not.
#[must_use]
pub fn unfit(rule: &Rule) -> Option<String> {
    if rule.parts().is_none() {
        return Some(format!(
            "{}: write it as <kind>:<name>, the kind one of {}",
            rule.pattern,
            KINDS.join(", ")
        ));
    }
    rule.reason
        .trim()
        .is_empty()
        .then(|| format!("{}: give a reason", rule.pattern))
}

impl ActionsConfig {
    /// The forbidden entry that names `kind` `name`, if one does.
    #[must_use]
    pub fn forbidden(&self, kind: &str, name: &str) -> Option<&Rule> {
        self.forbid.iter().find(|r| r.matches(kind, name))
    }

    /// Whether an allowed entry names `kind` `name` and no forbidden one does.
    #[must_use]
    pub fn allowed(&self, kind: &str, name: &str) -> bool {
        self.forbidden(kind, name).is_none() && self.allow.iter().any(|r| r.matches(kind, name))
    }

    /// The names of the entries of `kind` in `rules`.
    pub fn names<'a>(
        rules: &'a [Rule],
        kind: &'a str,
    ) -> impl Iterator<Item = (&'a str, &'a Rule)> {
        rules
            .iter()
            .filter_map(move |r| r.parts().filter(|(k, _)| *k == kind).map(|(_, n)| (n, r)))
    }

    /// The tool patterns of `rules` for the server `server` (`tool:<server glob>/<tool glob>`).
    #[must_use]
    pub fn tools_of(rules: &[Rule], server: &str) -> Vec<String> {
        Self::names(rules, "tool")
            .filter_map(|(name, _)| name.split_once('/'))
            .filter(|(s, _)| glob(s, server))
            .map(|(_, tool)| tool.to_string())
            .collect()
    }

    /// Whether the tool `tool` passes the tool patterns `allow` and `forbid` of its server: no forbidden pattern
    /// names it, and an allowed one does when there are any.
    #[must_use]
    pub fn tool_passes(allow: &[String], forbid: &[String], tool: &str) -> bool {
        !forbid.iter().any(|g| glob(g, tool))
            && (allow.is_empty() || allow.iter().any(|g| glob(g, tool)))
    }
}
