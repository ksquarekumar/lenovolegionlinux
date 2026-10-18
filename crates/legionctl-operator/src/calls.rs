//! The call record at dispatch (DESIGN/22 rule 3): an agent's operation, a brokered MCP tool call and a model call get
//! a call id and a row before they run (`legionctl_core::calls`). A retry with the same call id gets the recorded
//! result, never a second run; an agent's read with a cache time answers from the record.

use anyhow::Result;
use blake2::digest::consts::U16;
use blake2::{Blake2b, Digest};
use legionctl_api::{
    ApiError, CallBeginIn, CallEndIn, CallStart, Class, Code, OpInfo, Outcome, Principal, ToolCall,
    ToolCallRow,
};
use legionctl_core::calls::{Begun, Call};
use legionctl_core::db::Db;
use serde_json::Value;

/// The principal as the record keeps it.
pub(crate) fn principal_key(principal: &Principal) -> String {
    match principal {
        Principal::User => "user".into(),
        Principal::Session(id) => format!("session:{id}"),
        Principal::Agent(id) => format!("agent:{id}"),
    }
}

/// BLAKE2b-128 of a JSON value, hex.
pub(crate) fn hash(value: &Value) -> String {
    format!("{:x}", Blake2b::<U16>::digest(value.to_string().as_bytes()))
}

/// The caller's toolsets as one text: a cached answer is only for the scope it was made in.
fn scope_key(principal: &Principal) -> Result<String> {
    let Some(id) = principal.agent() else {
        return Ok(String::new());
    };
    let mut sets: Vec<String> = crate::engine::dossiers()?
        .scope(id)?
        .sets
        .into_iter()
        .map(|g| g.name)
        .collect();
    sets.sort();
    Ok(sets.join(","))
}

fn coded(code: Code, message: String) -> ApiError {
    ApiError {
        code,
        message,
        op: String::new(),
    }
}

fn internal(e: &anyhow::Error) -> ApiError {
    coded(
        legionctl_api::code_of(e),
        format!("the call record: {}", legionctl_core::error::message_of(e)),
    )
}

/// The outcome of an answer as the record keeps it.
fn outcome_of(answer: &Result<Value, ApiError>) -> &'static str {
    match answer {
        Ok(_) => "ok",
        Err(e) if e.code == Code::Denied => "refused",
        Err(e) if matches!(e.code, Code::Failed | Code::Internal | Code::Unavailable) => "failed",
        Err(_) => "error",
    }
}

/// A recorded operation call while it runs.
pub(crate) struct OpCall {
    pub(crate) id: String,
    keep: bool,
}

/// What dispatch does with a call after its start in the record.
pub(crate) enum Start {
    /// Run it; end it with [`end_op`].
    Run(OpCall),
    /// Answer with the recorded (or cached) answer.
    Answer(String, Result<Value, ApiError>),
}

/// Whether the record keeps this call: every public operation of an agent, and the user's changes (the user's reads,
/// such as the app's polls, are not kept).
fn recorded(op: &OpInfo, principal: &Principal) -> bool {
    !op.internal() && (!matches!(principal, Principal::User) || op.class != Class::Read)
}

/// Start an operation call in the record: a new row, the recorded answer of a call id that ended, a refusal for a
/// call id that has no end or is of another call, or a cached answer of a read within its cache time. `None`: the
/// record does not keep this call.
///
/// # Errors
/// The record cannot be written, or the call id is open or of another call.
pub(crate) fn begin_op(
    op: &OpInfo,
    (principal, folder): (&Principal, Option<&std::path::Path>),
    given: Option<&str>,
    input: &Value,
) -> Result<Option<Start>, ApiError> {
    if !recorded(op, principal) {
        return Ok(None);
    }
    let id = match given {
        Some(id) => id.to_string(),
        None => legionctl_core::calls::new_id().map_err(|e| internal(&e))?,
    };
    let (who, hash, scope) = (
        principal_key(principal),
        // the same input in another folder is another call
        hash(&serde_json::json!([folder, input])),
        scope_key(principal).map_err(|e| internal(&e))?,
    );
    let call = Call {
        call: &id,
        principal: &who,
        session: principal.agent().unwrap_or_default(),
        kind: "op",
        server: "legionctl",
        name: op.name,
        input_hash: &hash,
        scope: &scope,
    };
    let db = Db::open_default().map_err(|e| internal(&e))?;
    match db.call_begin(&call).map_err(|e| internal(&e))? {
        Begun::New => {}
        Begun::Ended { output, .. } => return Ok(Some(Start::Answer(id, replay(output)))),
        Begun::Open { started } => {
            return Err(coded(
                Code::Conflict,
                format!(
                    "call {id} of {} started at {} and has no end (it still runs, or the operator stopped during it); \
                     legionctl does not run it twice: check what it did",
                    op.name,
                    legionctl_core::views::local(started / 1000)
                ),
            ));
        }
        Begun::Other => {
            return Err(coded(
                Code::InvalidInput,
                format!(
                    "call id {id} is of another call: a retry sends the same operation and input"
                ),
            ));
        }
    }
    if op.cache_s > 0 {
        let since = legionctl_core::calls::now_ms() - i64::from(op.cache_s) * 1000;
        if let Some(output) = db.call_cached(&call, since).map_err(|e| internal(&e))? {
            let value: Value = serde_json::from_str(&output).unwrap_or(Value::Null);
            db.call_end(&id, "cached", None, output.len() as u64)
                .map_err(|e| internal(&e))?;
            return Ok(Some(Start::Answer(id, Ok(value))));
        }
    }
    Ok(Some(Start::Run(OpCall {
        id,
        keep: op.class != Class::Read || op.cache_s > 0,
    })))
}

/// The answer that the record kept for a call: its output, or its error envelope.
fn replay(output: Option<String>) -> Result<Value, ApiError> {
    let value: Value = output
        .and_then(|o| serde_json::from_str(&o).ok())
        .unwrap_or(Value::Null);
    match serde_json::from_value::<ApiError>(value["error"].clone()) {
        Ok(error) => Err(error),
        Err(_) => Ok(value),
    }
}

/// End `call` with its answer (kept for a retry of a change, and for the cache of a read with a cache time).
pub(crate) fn end_op(call: &OpCall, answer: &Result<Value, ApiError>) {
    let body = match answer {
        Ok(output) => output.clone(),
        Err(error) => serde_json::json!({ "error": error }),
    };
    let text = body.to_string();
    let kept = call.keep.then_some(text.as_str());
    if let Err(e) = Db::open_default()
        .and_then(|db| db.call_end(&call.id, outcome_of(answer), kept, text.len() as u64))
    {
        eprintln!(
            "operator: the end of call {} is not recorded: {e:#}",
            call.id
        );
    }
}

/// Start a brokered tool call (`internal.calls.begin`).
///
/// # Errors
/// The record cannot be written, or the call id is open or of another call.
pub(crate) fn begin_tool(principal: &Principal, input: &CallBeginIn) -> Result<CallStart> {
    let id = match &input.call {
        Some(id) => id.clone(),
        None => legionctl_core::calls::new_id()?,
    };
    let who = principal_key(principal);
    let name = format!("{}/{}", input.server, input.tool);
    let session = principal.agent().unwrap_or(&input.session.0);
    let call = Call {
        call: &id,
        principal: &who,
        session,
        kind: "tool",
        server: &input.server,
        name: &name,
        input_hash: &input.args_hash,
        scope: "",
    };
    let db = Db::open_default()?;
    match db.call_begin(&call)? {
        Begun::New if input.refused => {
            db.call_end(&id, "refused", None, 0)?;
            Ok(CallStart {
                call: id,
                replay: None,
            })
        }
        Begun::New => Ok(CallStart {
            call: id,
            replay: None,
        }),
        Begun::Ended { output, .. } => Ok(CallStart {
            call: id,
            replay: Some(
                output
                    .and_then(|o| serde_json::from_str(&o).ok())
                    .unwrap_or(Value::Null),
            ),
        }),
        Begun::Open { .. } => Err(coded(
            Code::Conflict,
            format!("call {id} of {name} has no end: legionctl does not run it twice"),
        )
        .into()),
        Begun::Other => Err(coded(
            Code::InvalidInput,
            format!("call id {id} is of another call: a retry sends the same tool and arguments"),
        )
        .into()),
    }
}

/// End a brokered tool call (`internal.calls.end`).
///
/// # Errors
/// The record cannot be written.
pub(crate) fn end_tool(input: &CallEndIn) -> Result<()> {
    let outcome = serde_json::to_value(input.outcome)?;
    let output = input.output.as_ref().map(Value::to_string);
    Db::open_default()?.call_end(
        &input.call,
        outcome.as_str().unwrap_or("ok"),
        output.as_deref(),
        input.bytes,
    )
}

/// A row of the record as `tools calls` lists it.
///
/// # Errors
/// Its outcome is not one the record writes.
pub(crate) fn row(r: legionctl_core::calls::Row) -> Result<ToolCallRow> {
    let outcome: Outcome =
        serde_json::from_value(Value::String(r.outcome.unwrap_or_else(|| "running".into())))?;
    let ms = r.ended.map_or(0, |e| e - r.started);
    let tool = if r.kind == "tool" {
        r.name
            .split_once('/')
            .map_or(r.name.clone(), |(_, t)| t.to_string())
    } else {
        r.name
    };
    Ok(ToolCallRow {
        id: r.id,
        call_id: r.call,
        kind: r.kind,
        at: r.started,
        call: ToolCall {
            session: legionctl_api::SessionId(r.session),
            server: r.server,
            tool,
            args_hash: r.input_hash,
            ms: u64::try_from(ms).unwrap_or(0),
            bytes: u64::try_from(r.bytes).unwrap_or(0),
            outcome,
        },
    })
}
