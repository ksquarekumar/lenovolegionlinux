//! rlcd (todo-138): System One decision models (Jev and models like it). One typed question model (choice with
//! options and their meanings, yes-no, score) and its answers (probabilities, confidence, usage and cost) on two wire
//! shapes: System One (`OpenRouter` `POST /api/alpha/decisions`: questions keyed by id) and `OpenAI`
//! (`POST /v1/decisions`: questions in order). Each decision is stored with its later outcome (the ACP answer it
//! woke, the next gate result) as a label for evaluation. A failed call fails its step; no other backend runs in its
//! place.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail, ensure};
use rusqlite::{OptionalExtension, params};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::db::now;
use crate::dossier::Dossiers;

/// The decisions table (operator.db v13), and the error of a cost row (a failed rlcd step).
macro_rules! tables {
    () => {
        "
     -- rlcd decisions (todo-138): path observer (a member's wake) or ask (`rlcd ask`); questions and answers as JSON;
     -- action: the answer of the question action of a wake; outcomes: acp (the answer of the wake it escalated to),
     -- gate (the next gate result of its thread)
     CREATE TABLE rlcd_decisions (
        id            INTEGER PRIMARY KEY,
        ts            INTEGER NOT NULL,
        path          TEXT NOT NULL,
        thread        INTEGER,
        member        TEXT,
        backend       TEXT NOT NULL,
        model         TEXT NOT NULL,
        questions     TEXT NOT NULL,
        answers       TEXT,
        action        TEXT,
        error         TEXT,
        input_tokens  INTEGER NOT NULL DEFAULT 0,
        cost_usd      REAL NOT NULL DEFAULT 0,
        ms            INTEGER NOT NULL,
        acp           TEXT,
        gate          TEXT
     );
     ALTER TABLE costs ADD COLUMN error TEXT;"
    };
}
pub(crate) use tables;

/// The wire shape of a backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Shape {
    /// `{model, state, questions: {<id>: {type: choice|noul|score, instructions, criteria}}}`.
    SystemOne,
    /// `{model, input, questions: [{type: choice|predicate|score, instructions, choices: [{value, description}]}]}`.
    Openai,
}

/// One decision backend (`rlcd.backends.<name>`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Backend {
    pub shape: Shape,
    /// The decisions endpoint.
    pub url: String,
    pub model: String,
    /// The keyring entry of its API key: `secret-tool lookup legionctl-rlcd <keyring>`.
    pub keyring: String,
    /// A call that takes longer fails its step.
    pub timeout_ms: u64,
}

/// The rlcd config: the backend that a member's rlcd and `rlcd ask` use when they name none, and the backends.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RlcdConfig {
    pub backend: String,
    pub backends: BTreeMap<String, Backend>,
}

/// One typed question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum Question {
    /// One of the options; option -> its meaning.
    Choice {
        instructions: String,
        options: BTreeMap<String, String>,
    },
    /// The probability of yes (System One `noul`, `OpenAI` `predicate`).
    Yesno { instructions: String },
    /// A number.
    Score { instructions: String },
}

/// The answer to one question.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Answer {
    Choice {
        choice: String,
        /// Option -> its probability.
        probabilities: BTreeMap<String, f64>,
        confidence: Option<f64>,
    },
    Yesno {
        /// The probability of yes.
        yes: f64,
    },
    Score {
        score: f64,
    },
}

/// A backend's answers to the questions of one call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Decision {
    /// Question id -> its answer.
    pub answers: BTreeMap<String, Answer>,
    pub input_tokens: u64,
    pub cost_usd: f64,
}

/// The most options a model takes in one choice question (SPIKE-rlcd-apis); none: not known. A model that takes yes-no
/// questions only has 0.
fn option_limit(model: &str) -> Option<usize> {
    [
        ("typesafe/jev", 255),
        ("upstage/solar-decide", 26),
        ("together/tev", 20),
        ("respan/span-01", 0),
    ]
    .into_iter()
    .find(|(prefix, _)| model.starts_with(prefix))
    .map(|(_, n)| n)
}

/// Refuse questions that the model cannot take: no question, a choice with no option, more options than its limit.
///
/// # Errors
/// Which question, and why.
pub fn check(model: &str, questions: &BTreeMap<String, Question>) -> Result<()> {
    ensure!(!questions.is_empty(), "no question");
    for (id, q) in questions {
        if let Question::Choice { options, .. } = q {
            ensure!(!options.is_empty(), "question {id}: a choice needs options");
            if let Some(limit) = option_limit(model) {
                ensure!(
                    options.len() <= limit,
                    "question {id}: {model} takes at most {limit} options in a choice, not {}",
                    options.len()
                );
            }
        }
    }
    Ok(())
}

/// A question from the command line: `<id>:choice:<a>[=<meaning>],<b>...[:<instructions>]`,
/// `<id>:yesno[:<instructions>]` or `<id>:score[:<instructions>]`. Without instructions, the id is the instructions.
///
/// # Errors
/// The text has no id, an unknown type, or a choice without options.
pub fn parse_question(text: &str) -> Result<(String, Question)> {
    let mut parts = text.splitn(3, ':');
    let (Some(id), Some(kind)) = (parts.next().filter(|i| !i.is_empty()), parts.next()) else {
        bail!(
            "question {text:?}: use <id>:choice:<a>,<b>[:<instructions>], <id>:yesno or <id>:score"
        );
    };
    let rest = parts.next();
    let question = match kind {
        "choice" => {
            let (options, instructions) = rest
                .map(|r| r.split_once(':').unwrap_or((r, id)))
                .with_context(|| {
                    format!("question {id}: a choice needs options: {id}:choice:a,b")
                })?;
            Question::Choice {
                instructions: instructions.into(),
                options: options
                    .split(',')
                    .filter(|o| !o.is_empty())
                    .map(|o| {
                        let (name, meaning) = o.split_once('=').unwrap_or((o, o));
                        (name.to_string(), meaning.to_string())
                    })
                    .collect(),
            }
        }
        "yesno" => Question::Yesno {
            instructions: rest.unwrap_or(id).into(),
        },
        "score" => Question::Score {
            instructions: rest.unwrap_or(id).into(),
        },
        other => bail!("question {id}: type {other} is not choice, yesno or score"),
    };
    Ok((id.into(), question))
}

/// The request body of `shape` for `state`.
#[must_use]
pub fn request(
    shape: Shape,
    model: &str,
    state: &str,
    questions: &BTreeMap<String, Question>,
) -> Value {
    match shape {
        Shape::SystemOne => {
            let keyed: Map<String, Value> = questions
                .iter()
                .map(|(id, q)| {
                    let wire = match q {
                        Question::Choice {
                            instructions,
                            options,
                        } => json!({"type": "choice", "instructions": instructions, "criteria": options}),
                        Question::Yesno { instructions } => {
                            json!({"type": "noul", "instructions": instructions})
                        }
                        Question::Score { instructions } => {
                            json!({"type": "score", "instructions": instructions})
                        }
                    };
                    (id.clone(), wire)
                })
                .collect();
            json!({"model": model, "state": state, "questions": keyed})
        }
        Shape::Openai => {
            let list: Vec<Value> = questions
                .values()
                .map(|q| match q {
                    Question::Choice {
                        instructions,
                        options,
                    } => json!({"type": "choice", "instructions": instructions, "choices": options
                        .iter()
                        .map(|(value, description)| json!({"value": value, "description": description}))
                        .collect::<Vec<_>>()}),
                    Question::Yesno { instructions } => {
                        json!({"type": "predicate", "instructions": instructions})
                    }
                    Question::Score { instructions } => {
                        json!({"type": "score", "instructions": instructions})
                    }
                })
                .collect();
            json!({"model": model, "input": state, "questions": list})
        }
    }
}

/// The answers of a response body: each question answered in its type, a choice one of its options.
// shortcut: the OpenAI answer shape (answers in order; predicate `probability`, score `score`) and the System One
// score are not measured live yet (SPIKE-rlcd-apis: the OpenAI call failed with 500); check them at the first answer
///
/// # Errors
/// The body is not an answer to these questions.
pub fn parse(
    shape: Shape,
    questions: &BTreeMap<String, Question>,
    body: &Value,
) -> Result<Decision> {
    let wire: Vec<(&String, &Question, &Value)> = match shape {
        Shape::SystemOne => questions
            .iter()
            .map(|(id, q)| (id, q, &body["answers"][id.as_str()]))
            .collect(),
        Shape::Openai => {
            let list = body["answers"]
                .as_array()
                .context("the answer has no answers array")?;
            ensure!(
                list.len() == questions.len(),
                "{} answers to {} questions",
                list.len(),
                questions.len()
            );
            questions
                .iter()
                .zip(list)
                .map(|((id, q), a)| (id, q, a))
                .collect()
        }
    };
    let mut answers = BTreeMap::new();
    for (id, question, a) in wire {
        let number = |key: &str| {
            a[key]
                .as_f64()
                .with_context(|| format!("question {id}: no number {key} in {a}"))
        };
        let answer = match question {
            Question::Choice { options, .. } => {
                let choice = a["choice"]
                    .as_str()
                    .with_context(|| format!("question {id}: no choice in {a}"))?;
                ensure!(
                    options.contains_key(choice),
                    "question {id}: {choice} is not an option"
                );
                Answer::Choice {
                    choice: choice.into(),
                    probabilities: serde_json::from_value(a["probabilities"].clone())
                        .with_context(|| format!("question {id}: probabilities"))?,
                    confidence: a["confidence"].as_f64(),
                }
            }
            Question::Yesno { .. } => Answer::Yesno {
                yes: number(if shape == Shape::SystemOne {
                    "noul"
                } else {
                    "probability"
                })?,
            },
            Question::Score { .. } => Answer::Score {
                score: number("score")?,
            },
        };
        answers.insert(id.clone(), answer);
    }
    let usage = &body["usage"];
    Ok(Decision {
        answers,
        input_tokens: usage["input_tokens"].as_u64().unwrap_or(0),
        cost_usd: usage["cost"].as_f64().unwrap_or(0.0),
    })
}

/// One line per answer: `action escalate p 0.74 (note 0.24, ignore 0.01), confidence 0.62`, `stuck yes 0.02`.
#[must_use]
pub fn brief(decision: &Decision) -> String {
    decision
        .answers
        .iter()
        .map(|(id, a)| match a {
            Answer::Choice {
                choice,
                probabilities,
                confidence,
            } => {
                let p = |o: &str| probabilities.get(o).copied().unwrap_or(0.0);
                let others: Vec<String> = probabilities
                    .keys()
                    .filter(|o| *o != choice)
                    .map(|o| format!("{o} {:.2}", p(o)))
                    .collect();
                let confidence =
                    confidence.map_or_else(String::new, |c| format!(", confidence {c:.2}"));
                format!(
                    "{id} {choice} p {:.2} ({}){confidence}",
                    p(choice),
                    others.join(", ")
                )
            }
            Answer::Yesno { yes } => format!("{id} yes {yes:.2}"),
            Answer::Score { score } => format!("{id} score {score:.2}"),
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Key prefixes of well-known API keys and tokens.
const KEY_PREFIXES: [&str; 13] = [
    "sk-",
    "sk_",
    "ghp_",
    "gho_",
    "ghs_",
    "github_pat_",
    "xoxb-",
    "xoxp-",
    "glpat-",
    "hf_",
    "AKIA",
    "AIza",
    "eyJ",
];

/// `text` with secrets removed before it leaves the machine: each of `secrets` (the backend's key, the values of the
/// operator's `*_KEY`, `*_TOKEN` and `*_SECRET` variables), and each word of 16 characters or more that starts like a
/// well-known key, become `<secret>`.
#[must_use]
pub fn redact(text: &str, secrets: &[String]) -> String {
    let mut out = text.to_string();
    for s in secrets.iter().filter(|s| s.len() >= 8) {
        out = out.replace(s.as_str(), "<secret>");
    }
    let word = |c: char| c.is_ascii_alphanumeric() || "-_.".contains(c);
    let mut result = String::with_capacity(out.len());
    let mut rest = out.as_str();
    while let Some(start) = rest.find(word) {
        result.push_str(&rest[..start]);
        let tail = &rest[start..];
        let end = tail.find(|c| !word(c)).unwrap_or(tail.len());
        let token = &tail[..end];
        if token.len() >= 16 && KEY_PREFIXES.iter().any(|p| token.starts_with(p)) {
            result.push_str("<secret>");
        } else {
            result.push_str(token);
        }
        rest = &tail[end..];
    }
    result.push_str(rest);
    result
}

/// The values of the environment's `*_KEY`, `*_TOKEN` and `*_SECRET` variables.
#[must_use]
pub fn env_secrets() -> Vec<String> {
    std::env::vars()
        .filter(|(k, _)| ["_KEY", "_TOKEN", "_SECRET"].iter().any(|s| k.ends_with(s)))
        .map(|(_, v)| v)
        .collect()
}

/// A stored decision with its outcomes.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Stored {
    pub id: i64,
    pub ts: i64,
    /// observer (a member's wake) or ask (`rlcd ask`).
    pub path: String,
    pub thread: Option<i64>,
    pub member: Option<String>,
    pub backend: String,
    pub model: String,
    /// Question id -> its question.
    pub questions: Value,
    /// Question id -> its answer; none when the call failed.
    pub answers: Option<Value>,
    /// The answer to the question `action` of a wake: ignore, note or escalate.
    pub action: Option<String>,
    pub error: Option<String>,
    pub input_tokens: i64,
    pub cost_usd: f64,
    pub ms: i64,
    /// The outcome of the ACP wake it escalated to: `{"ok": true, "text": ...}` or `{"error": ...}`.
    pub acp: Option<Value>,
    /// The next gate result of its thread: `{"gate": ..., "passed": bool, "open": [...]}`.
    pub gate: Option<Value>,
}

/// What one call gave, to store.
pub struct Call<'a> {
    pub path: &'a str,
    /// The thread and member of a wake.
    pub member: Option<(i64, &'a str)>,
    pub backend: &'a str,
    pub model: &'a str,
    pub questions: &'a BTreeMap<String, Question>,
    pub result: &'a Result<Decision, String>,
    pub ms: u64,
}

impl Dossiers {
    /// Store one decision (or its failure); its id.
    ///
    /// # Errors
    /// The database write fails.
    pub fn decision_add(&self, c: &Call) -> Result<i64> {
        let (answers, action, error, tokens, cost) = match c.result {
            Ok(d) => (
                Some(serde_json::to_string(&d.answers)?),
                match d.answers.get("action") {
                    Some(Answer::Choice { choice, .. }) => Some(choice.clone()),
                    _ => None,
                },
                None,
                i64::try_from(d.input_tokens)?,
                d.cost_usd,
            ),
            Err(e) => (None, None, Some(e.clone()), 0, 0.0),
        };
        self.db().execute(
            "INSERT INTO rlcd_decisions (ts, path, thread, member, backend, model, questions, answers, action,
             error, input_tokens, cost_usd, ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                now(),
                c.path,
                c.member.map(|m| m.0),
                c.member.map(|m| m.1),
                c.backend,
                c.model,
                serde_json::to_string(c.questions)?,
                answers,
                action,
                error,
                tokens,
                cost,
                i64::try_from(c.ms)?
            ],
        )?;
        Ok(self.db().last_insert_rowid())
    }

    /// The outcome of the ACP wake that decision `id` escalated to.
    ///
    /// # Errors
    /// The database write fails.
    pub fn decision_acp(&self, id: i64, outcome: &Value) -> Result<()> {
        self.db().execute(
            "UPDATE rlcd_decisions SET acp = ?2 WHERE id = ?1",
            params![id, outcome.to_string()],
        )?;
        Ok(())
    }

    /// The stored decisions, newest first (with `id`, that one only).
    ///
    /// # Errors
    /// The database read fails.
    pub fn decisions(&self, limit: u32, id: Option<i64>) -> Result<Vec<Stored>> {
        let value = |s: String| -> rusqlite::Result<Value> {
            serde_json::from_str(&s).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })
        };
        let json = |s: Option<String>| s.map(value).transpose();
        self.db()
            .prepare(
                "SELECT id, ts, path, thread, member, backend, model, questions, answers, action, error,
                 input_tokens, cost_usd, ms, acp, gate FROM rlcd_decisions WHERE ?2 IS NULL OR id = ?2
                 ORDER BY id DESC LIMIT ?1",
            )?
            .query_map(params![limit, id], |r| {
                Ok(Stored {
                    id: r.get(0)?,
                    ts: r.get(1)?,
                    path: r.get(2)?,
                    thread: r.get(3)?,
                    member: r.get(4)?,
                    backend: r.get(5)?,
                    model: r.get(6)?,
                    questions: value(r.get(7)?)?,
                    answers: json(r.get(8)?)?,
                    action: r.get(9)?,
                    error: r.get(10)?,
                    input_tokens: r.get(11)?,
                    cost_usd: r.get(12)?,
                    ms: r.get(13)?,
                    acp: json(r.get(14)?)?,
                    gate: json(r.get(15)?)?,
                })
            })?
            .collect::<Result<_, _>>()
            .map_err(Into::into)
    }
}

/// The next gate result of a thread is the outcome of each of its decisions that has none yet.
pub(crate) fn gate_outcome(
    db: &rusqlite::Connection,
    thread: i64,
    gate: &str,
    open: &[String],
) -> Result<()> {
    let waiting: Option<i64> = db
        .query_row(
            "SELECT id FROM rlcd_decisions WHERE thread = ?1 AND gate IS NULL LIMIT 1",
            [thread],
            |r| r.get(0),
        )
        .optional()?;
    if waiting.is_some() {
        db.execute(
            "UPDATE rlcd_decisions SET gate = ?2 WHERE thread = ?1 AND gate IS NULL",
            params![
                thread,
                json!({"gate": gate, "passed": open.is_empty(), "open": open}).to_string()
            ],
        )?;
    }
    Ok(())
}
