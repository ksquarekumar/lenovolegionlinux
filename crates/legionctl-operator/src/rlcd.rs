//! rlcd calls (todo-138): one HTTP call per decision to the configured backend, with the key from the keyring
//! (`secret-tool lookup legionctl-rlcd <entry>`) and the state's secrets removed. A failed or timed-out call fails its
//! step with its error; no other backend runs in its place. The observer step of a wake and `rlcd ask` use it; each
//! decision is stored.

use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use legionctl_core::archetypes::MemberRlcd;
use legionctl_core::config::{Dirs, Resolved, resolve};
use legionctl_core::dossier::Dossiers;
use legionctl_core::pool::{RlcdEnd, Wake};
use legionctl_core::rlcd::{
    Answer, Backend, Call, Decision, Question, Stored, brief, check, env_secrets, parse, redact,
    request,
};
use serde_json::json;

/// The backend `name` (none: `rlcd.backend`). A repository layer may select a backend, never set its url, keyring or
/// shape: it could send the key elsewhere.
///
/// # Errors
/// No such backend, or the repository layer set one of those fields.
pub fn backend(resolved: &Resolved, name: Option<&str>) -> Result<(String, Backend)> {
    let name = name.unwrap_or(&resolved.config.rlcd.backend).to_string();
    let found = resolved
        .config
        .rlcd
        .backends
        .get(&name)
        .with_context(|| format!("no rlcd.backends.{name}"))?
        .clone();
    for field in ["url", "keyring", "shape"] {
        let key = format!("rlcd.backends.{name}.{field}");
        if let Some(set) = resolved
            .trace
            .get(&key)
            .and_then(|t| t.last())
            .filter(|s| s.source.layer == "repo")
        {
            bail!(
                "{key} is set by the repository layer ({}): only the defaults, /etc/legionctl and the user layer may \
                 set a backend's url, keyring or shape",
                set.source.file
            );
        }
    }
    Ok((name, found))
}

/// The API key of keyring entry `entry`.
///
/// # Errors
/// secret-tool does not run, or the entry is missing.
pub fn keyring(entry: &str) -> Result<String> {
    let out = std::process::Command::new("secret-tool")
        .args(["lookup", "legionctl-rlcd", entry])
        .output()
        .context("running secret-tool")?;
    let key = String::from_utf8_lossy(&out.stdout).trim().to_string();
    ensure!(
        out.status.success() && !key.is_empty(),
        "no keyring entry legionctl-rlcd {entry} (secret-tool store --label='legionctl rlcd {entry}' legionctl-rlcd {entry})"
    );
    Ok(key)
}

fn http() -> &'static reqwest::Client {
    static HTTP: OnceLock<reqwest::Client> = OnceLock::new();
    HTTP.get_or_init(reqwest::Client::new)
}

/// One decision: the questions on `state` (its secrets removed), within the backend's timeout.
///
/// # Errors
/// The model cannot take the questions, the call fails or times out, the backend answers with an error, or its answer
/// does not answer the questions.
pub async fn call(
    b: &Backend,
    key: &str,
    state: &str,
    questions: &BTreeMap<String, Question>,
) -> Result<Decision> {
    check(&b.model, questions)?;
    let mut secrets = env_secrets();
    secrets.push(key.to_string());
    let body = request(b.shape, &b.model, &redact(state, &secrets), questions);
    let response = http()
        .post(&b.url)
        .bearer_auth(key)
        .timeout(Duration::from_millis(b.timeout_ms))
        .json(&body)
        .send()
        .await
        .with_context(|| format!("{} (timeout {} ms)", b.url, b.timeout_ms))?;
    let status = response.status();
    let text = response.text().await.context("reading the answer")?;
    ensure!(
        status.is_success(),
        "{} answered {status}: {}",
        b.url,
        text.chars().take(300).collect::<String>()
    );
    let value: serde_json::Value = serde_json::from_str(&text).context("the answer is not JSON")?;
    parse(b.shape, questions, &value)
}

/// What the call took, and its result as the store keeps it.
pub struct Asked {
    pub result: Result<Decision, String>,
    pub ms: u64,
}

pub fn ask(
    runtime: &tokio::runtime::Handle,
    (b, key): (&Backend, &str),
    state: &str,
    questions: &BTreeMap<String, Question>,
) -> Asked {
    let started = Instant::now();
    let result = runtime
        .block_on(call(b, key, state, questions))
        .map_err(|e| format!("{e:#}"));
    Asked {
        result,
        ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    }
}

/// `rlcd ask`: one decision, stored with path ask.
///
/// # Errors
/// A question does not parse, the backend or its key is missing, or the call fails (the failure is stored first).
pub fn ask_op(
    runtime: &tokio::runtime::Handle,
    store: &Dossiers,
    input: &legionctl_api::RlcdAskIn,
) -> Result<Stored> {
    let questions = input
        .questions
        .iter()
        .map(|q| legionctl_core::rlcd::parse_question(q))
        .collect::<Result<BTreeMap<_, _>>>()?;
    let resolved = resolve(&Dirs::from_env(), Some(input.repo.get()?))?;
    let (name, b) = backend(&resolved, input.backend.as_deref())?;
    let key = match &input.key {
        Some(key) => key.clone(),
        None => keyring(&b.keyring)?,
    };
    let asked = ask(runtime, (&b, &key), &input.state.0, &questions);
    let id = store.decision_add(&Call {
        path: "ask",
        member: None,
        backend: &name,
        model: &b.model,
        questions: &questions,
        result: &asked.result,
        ms: asked.ms,
    })?;
    if let Err(e) = &asked.result {
        bail!("rlcd {name}: {e}");
    }
    store
        .decisions(1, Some(id))?
        .pop()
        .context("the stored decision is gone")
}

/// What the rlcd step of a wake decided.
pub enum Step {
    /// The next step of the route runs, with this brief; the decision's id gets its outcome.
    Escalate { decision: i64, brief: String },
    /// The wake ended here: ignore, note, or a failed step (`again`: the member wakes again now).
    Ended { again: bool },
}

/// The rlcd step of member `w`'s wake: its questions on the wake's events. The question action decides; with
/// `escalate_at`, an escalate probability of at least that percent escalates too. `next`: a step follows.
///
/// # Errors
/// The member's rlcd config does not parse, or the database fails. A failed call is no error: it ends the wake as a
/// failed step.
pub fn observe(
    runtime: &tokio::runtime::Handle,
    store: &Dossiers,
    w: &Wake,
    next: bool,
) -> Result<Step> {
    let config: MemberRlcd =
        serde_json::from_value(w.fields["rlcd"].clone()).context("the member's rlcd config")?;
    let started = Instant::now();
    let chosen = resolve(&Dirs::from_env(), Some(&w.workdir))
        .and_then(|resolved| backend(&resolved, config.backend.as_deref()))
        .and_then(|(name, b)| Ok((keyring(&b.keyring)?, name, b)));
    let (name, model, asked) = match chosen {
        Ok((key, name, b)) => {
            let asked = ask(runtime, (&b, &key), &w.prompt, &config.questions);
            (name, b.model, asked)
        }
        Err(e) => (
            config.backend.clone().unwrap_or_default(),
            String::new(),
            Asked {
                result: Err(format!("{e:#}")),
                ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            },
        ),
    };
    let decision = store.decision_add(&Call {
        path: "observer",
        member: Some((w.thread, &w.member)),
        backend: &name,
        model: &model,
        questions: &config.questions,
        result: &asked.result,
        ms: asked.ms,
    })?;
    let (action, line, escalate) = match &asked.result {
        Ok(d) => {
            let Some(Answer::Choice {
                choice,
                probabilities,
                ..
            }) = d.answers.get("action")
            else {
                bail!("the member's rlcd questions have no choice action");
            };
            let over = config.escalate_at.is_some_and(|at| {
                probabilities.get("escalate").copied().unwrap_or(0.0) * 100.0 >= f64::from(at)
            });
            (Some(choice.clone()), brief(d), choice == "escalate" || over)
        }
        Err(_) => (None, String::new(), false),
    };
    let error = asked.result.as_ref().err().cloned();
    let event = json!({"decision": decision, "backend": name, "model": model, "action": action,
        "escalated": escalate, "brief": line, "error": error});
    let tokens = asked.result.as_ref().map_or(0, |d| d.input_tokens);
    let ends = !(escalate && next);
    let again = store.rlcd_end(
        w,
        &RlcdEnd {
            tokens,
            ms: asked.ms,
            error,
            event,
            ends,
        },
    )?;
    Ok(if ends {
        Step::Ended { again }
    } else {
        Step::Escalate {
            decision,
            brief: format!("rlcd ({name}, {model}) escalated this wake: {line}"),
        }
    })
}
