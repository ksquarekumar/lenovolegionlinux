//! The toolset scope of agent calls (DESIGN/22 rule 2): an agent (a sandboxed session or a harness agent) holds the
//! toolsets of its grant and the read class. A call outside them asks the user for the whole toolset that holds the
//! tool; the answer joins the agent's grant, so dispatch enforces it (review/16 C1). The user has no scope.

use anyhow::Result;
use legionctl_api::{ApiError, Class, Code, OPS, Principal, Scope, ScopeIn};
use legionctl_core::runnables::{holds, toolset_for};

/// The class of an operation as toolsets name it.
pub(crate) fn class_name(class: Class) -> &'static str {
    match class {
        Class::Read => "read",
        Class::Session => "session",
        Class::Workspace => "workspace",
        Class::Machine => "machine",
    }
}

/// The class of a tool name: an operation's, or none for a brokered tool.
fn class_of(name: &str) -> Option<&'static str> {
    OPS.iter()
        .find(|op| op.name == name)
        .map(|op| class_name(op.class))
}

fn denied(message: String) -> ApiError {
    ApiError {
        code: Code::Denied,
        message,
        op: String::new(),
    }
}

fn internal(e: &anyhow::Error) -> ApiError {
    ApiError {
        code: legionctl_api::code_of(e),
        message: format!("{e:#}"),
        op: String::new(),
    }
}

/// Whether agent `id` may run `name` now: in its scope (false), or after the user allowed the toolset that holds it
/// (true). `what` is the call in words, for the question.
///
/// # Errors
/// No toolset holds the tool, the user did not allow it or could not be asked, or the grant cannot be read or written.
pub(crate) async fn check(id: &str, name: &str, what: &str) -> Result<bool, ApiError> {
    let class = class_of(name);
    if class == Some("read") {
        return Ok(false);
    }
    let store = crate::engine::dossiers().map_err(|e| internal(&e))?;
    plugin_scope(&Principal::Agent(id.to_string()), id)?;
    let scope = store.scope(id).map_err(|e| internal(&e))?;
    if holds(&scope.manifest.toolverse, name, class) {
        return Ok(false);
    }
    let config = crate::engine::config(None)
        .map_err(|e| internal(&e))?
        .config;
    let Some(set) = toolset_for(&config, name, class) else {
        return Err(denied(format!(
            "{name} is in no toolset that legionctl may grant: add it to a capability set's toolverse and to \
             runnables.allow"
        )));
    };
    let short = &id[..id.len().min(8)];
    let question = format!(
        "Agent session {short} asks for the toolset {}: {}\nIts first use: {what}",
        set.set.name, set.set.description
    );
    match crate::desktop::ask("legionctl: an agent asks for a toolset", &question).await {
        Ok(true) => store
            .grant_toolset(id, set)
            .map(|()| true)
            .map_err(|e| internal(&e)),
        Ok(false) => Err(denied(format!(
            "{name} needs the toolset {}, and the user did not allow it",
            set.set.name
        ))),
        Err(e) => Err(denied(format!(
            "{name} needs the toolset {}, which the user must allow, but nobody could be asked: {e:#}",
            set.set.name
        ))),
    }
}

/// The harness agent of session `id` has `skills`: the toolsets they declare join its scope (no question: the skills
/// are the declaration). The hooks are the user's own process, so their session id is the agent's: at session start
/// the plugin's skills (the harness has them all from the plugin), and each skill the agent loads later (Claude Code's
/// `Skill` call). A legionctl session (sandboxed or a thread member) keeps the grant of its start as its ceiling.
///
/// # Errors
/// The grant cannot be read or written.
pub(crate) fn declare(principal: &Principal, id: &str, skills: &[String]) -> Result<()> {
    if !matches!(principal, Principal::Agent(_)) || id.is_empty() {
        return Ok(());
    }
    let store = crate::engine::dossiers()?;
    if store.session(id).is_ok() {
        return Ok(());
    }
    let config = crate::engine::config(None)?.config;
    for name in skills
        .iter()
        .flat_map(|s| legionctl_core::archetypes::skill_toolsets(s))
    {
        let grantable = config
            .capabilities
            .iter()
            .find(|l| l.set.name == name)
            .filter(|_| legionctl_core::runnables::refusal(&config, &name).is_none());
        if let Some(set) = grantable {
            store.grant_toolset(id, set)?;
        }
    }
    Ok(())
}

/// A harness session starts (its session-start hook): it has the plugin's skills, so their toolsets are its scope.
///
/// # Errors
/// The grant cannot be read or written.
pub(crate) fn session_start(principal: &Principal, input: &legionctl_api::HookRunIn) -> Result<()> {
    match &input.session {
        Some(session) if matches!(input.event, legionctl_api::Event::SessionStart) => declare(
            principal,
            &session.0,
            &legionctl_core::archetypes::plugin_skills(),
        ),
        _ => Ok(()),
    }
}

/// The agent ids of a client that knows no session (Codex starts its MCP servers and hooks with a cleared
/// environment): the plugin's own server and hooks, which have the plugin's skills.
const NO_SESSION: [&str; 2] = ["mcp", "hook"];

/// An agent that knows no session holds the toolsets of the plugin's skills, as a harness session does from its start.
fn plugin_scope(principal: &Principal, id: &str) -> Result<(), ApiError> {
    if NO_SESSION.contains(&id) {
        declare(principal, id, &legionctl_core::archetypes::plugin_skills())
            .map_err(|e| internal(&e))?;
    }
    Ok(())
}

/// The `runnables.scope` answer for `principal`.
///
/// # Errors
/// The grant or the config cannot be read.
pub(crate) fn scope(principal: &Principal, input: &ScopeIn) -> Result<Scope> {
    let Some(id) = principal.agent() else {
        return Ok(Scope::default());
    };
    plugin_scope(principal, id).map_err(anyhow::Error::new)?;
    let grant = crate::engine::dossiers()?.scope(id)?;
    let config = crate::engine::config(None)?.config;
    let names: Vec<String> = if input.tools.is_empty() {
        OPS.iter()
            .filter(|op| !op.internal())
            .map(|op| op.name.to_string())
            .collect()
    } else {
        input.tools.clone()
    };
    let missing = names
        .into_iter()
        .filter_map(|name| {
            let class = class_of(&name);
            (class != Some("read") && !holds(&grant.manifest.toolverse, &name, class)).then(|| {
                let set = toolset_for(&config, &name, class)
                    .map(|l| l.set.name.clone())
                    .unwrap_or_default();
                (name, set)
            })
        })
        .collect();
    Ok(Scope {
        sets: grant.sets.into_iter().map(|g| g.name).collect(),
        missing,
    })
}
