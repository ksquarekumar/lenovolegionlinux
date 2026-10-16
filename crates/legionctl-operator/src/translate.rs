//! Translation between the Anthropic Messages API and the `OpenAI` Chat Completions API (note 19, G2): requests,
//! JSON answers, errors and SSE streams. A request that one API cannot express in the other is refused with the
//! reason, never changed silently; `cache_control` hints are dropped, as they change cost, not the answer.

use std::fmt::Write as _;

use serde_json::{Map, Value, json};

/// Which way a call goes: the caller's API, then the provider's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction {
    /// An Anthropic caller (Claude Code) in front of an `OpenAI` model.
    MessagesToChat,
    /// An `OpenAI` caller (Codex, SDKs) in front of an Anthropic model.
    ChatToMessages,
}

fn unknown(request: &Value, known: &[&str]) -> Result<(), String> {
    let extra: Vec<&str> = request
        .as_object()
        .into_iter()
        .flat_map(Map::keys)
        .map(String::as_str)
        .filter(|k| !known.contains(k))
        .collect();
    if extra.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "the other API has no equivalent for: {}",
            extra.join(", ")
        ))
    }
}

/// The text of a string or of a list of text blocks/parts.
fn text_of(content: &Value, what: &str) -> Result<String, String> {
    match content {
        Value::String(s) => Ok(s.clone()),
        Value::Array(blocks) => blocks
            .iter()
            .map(|b| match b["type"].as_str() {
                Some("text") => Ok(b["text"].as_str().unwrap_or_default().to_string()),
                other => Err(format!(
                    "{what} has a {} block; only text translates",
                    other.unwrap_or("?")
                )),
            })
            .collect::<Result<Vec<_>, _>>()
            .map(|t| t.join("\n")),
        Value::Null => Ok(String::new()),
        _ => Err(format!("{what} is not text")),
    }
}

/// The caller's request in the provider's API.
///
/// # Errors
/// The request uses something the other API cannot express; the message names it.
pub fn request(direction: Direction, request: &Value) -> Result<Value, String> {
    match direction {
        Direction::MessagesToChat => messages_to_chat(request),
        Direction::ChatToMessages => chat_to_messages(request),
    }
}

fn messages_to_chat(request: &Value) -> Result<Value, String> {
    unknown(
        request,
        &[
            "model",
            "messages",
            "system",
            "max_tokens",
            "temperature",
            "top_p",
            "stop_sequences",
            "stream",
            "tools",
            "tool_choice",
            "metadata",
        ],
    )?;
    let mut messages = Vec::new();
    if let Some(system) = request.get("system") {
        messages.push(json!({"role": "system", "content": text_of(system, "system")?}));
    }
    for message in request["messages"]
        .as_array()
        .ok_or("messages is not a list")?
    {
        let role = message["role"].as_str().ok_or("a message has no role")?;
        let blocks = match &message["content"] {
            Value::String(text) => {
                messages.push(json!({"role": role, "content": text}));
                continue;
            }
            Value::Array(blocks) => blocks,
            _ => return Err("a message has no content".into()),
        };
        if role == "assistant" {
            messages.push(assistant_to_chat(blocks)?);
        } else {
            user_to_chat(blocks, &mut messages)?;
        }
    }
    let mut out = json!({"model": request["model"], "messages": messages});
    for (from, to) in [
        ("max_tokens", "max_completion_tokens"),
        ("temperature", "temperature"),
        ("top_p", "top_p"),
        ("stop_sequences", "stop"),
    ] {
        if let Some(v) = request.get(from) {
            out[to] = v.clone();
        }
    }
    if request["stream"] == true {
        out["stream"] = true.into();
        // the usage chunk: the gateway counts tokens and cost from it
        out["stream_options"] = json!({"include_usage": true});
    }
    if let Some(user) = request["metadata"].get("user_id") {
        out["user"] = user.clone();
    }
    if let Some(tools) = request.get("tools") {
        out["tools"] = tools
            .as_array()
            .ok_or("tools is not a list")?
            .iter()
            .map(|t| {
                if t.get("type").is_some_and(|k| k != "custom") {
                    return Err(format!(
                        "tool {} is an Anthropic server tool",
                        t["name"].as_str().unwrap_or("?")
                    ));
                }
                let mut function = json!({"name": t["name"], "parameters": t["input_schema"]});
                if let Some(d) = t.get("description") {
                    function["description"] = d.clone();
                }
                Ok(json!({"type": "function", "function": function}))
            })
            .collect::<Result<Vec<_>, String>>()?
            .into();
    }
    if let Some(choice) = request.get("tool_choice") {
        out["tool_choice"] = match choice["type"].as_str() {
            Some("auto") => "auto".into(),
            Some("any") => "required".into(),
            Some("none") => "none".into(),
            Some("tool") => json!({"type": "function", "function": {"name": choice["name"]}}),
            other => return Err(format!("tool_choice {}", other.unwrap_or("?"))),
        };
        if choice["disable_parallel_tool_use"] == true {
            out["parallel_tool_calls"] = false.into();
        }
    }
    Ok(out)
}

fn assistant_to_chat(blocks: &[Value]) -> Result<Value, String> {
    let mut text = Vec::new();
    let mut calls = Vec::new();
    for block in blocks {
        match block["type"].as_str() {
            Some("text") => text.push(block["text"].as_str().unwrap_or_default()),
            Some("tool_use") => calls.push(json!({
                "id": block["id"],
                "type": "function",
                "function": {"name": block["name"], "arguments": block["input"].to_string()},
            })),
            other => {
                return Err(format!(
                    "an assistant {} block cannot go to an OpenAI model",
                    other.unwrap_or("?")
                ));
            }
        }
    }
    let mut message = json!({"role": "assistant", "content": text.join("")});
    if !calls.is_empty() {
        message["tool_calls"] = calls.into();
    }
    Ok(message)
}

/// Tool results become `tool` messages, first, as `OpenAI` wants them right after the call; the rest is one user
/// message.
fn user_to_chat(blocks: &[Value], messages: &mut Vec<Value>) -> Result<(), String> {
    let mut parts = Vec::new();
    for block in blocks {
        match block["type"].as_str() {
            Some("text") => parts.push(json!({"type": "text", "text": block["text"]})),
            Some("image") => {
                let source = &block["source"];
                let url = match source["type"].as_str() {
                    Some("base64") => format!(
                        "data:{};base64,{}",
                        source["media_type"].as_str().unwrap_or_default(),
                        source["data"].as_str().unwrap_or_default()
                    ),
                    Some("url") => source["url"].as_str().unwrap_or_default().to_string(),
                    other => return Err(format!("an image from {}", other.unwrap_or("?"))),
                };
                parts.push(json!({"type": "image_url", "image_url": {"url": url}}));
            }
            Some("tool_result") => {
                let mut content = text_of(&block["content"], "a tool result")?;
                // OpenAI tool messages have no error flag: say it in the text
                if block["is_error"] == true {
                    content.insert_str(0, "[tool error] ");
                }
                messages.push(json!({"role": "tool", "tool_call_id": block["tool_use_id"], "content": content}));
            }
            other => {
                return Err(format!(
                    "a user {} block cannot go to an OpenAI model",
                    other.unwrap_or("?")
                ));
            }
        }
    }
    if !parts.is_empty() {
        messages.push(json!({"role": "user", "content": parts}));
    }
    Ok(())
}

fn chat_to_messages(request: &Value) -> Result<Value, String> {
    unknown(
        request,
        &[
            "model",
            "messages",
            "max_tokens",
            "max_completion_tokens",
            "temperature",
            "top_p",
            "stop",
            "stream",
            "stream_options",
            "tools",
            "tool_choice",
            "parallel_tool_calls",
            "user",
            "n",
        ],
    )?;
    if request.get("n").is_some_and(|n| n != 1) {
        return Err("n above 1: Anthropic models give one answer".into());
    }
    let mut system = Vec::new();
    let mut messages: Vec<Value> = Vec::new();
    for message in request["messages"]
        .as_array()
        .ok_or("messages is not a list")?
    {
        let (role, blocks) = match message["role"].as_str() {
            Some("system" | "developer") => {
                system.push(text_of(&message["content"], "a system message")?);
                continue;
            }
            Some("assistant") => ("assistant", assistant_to_messages(message)?),
            Some("tool") => (
                "user",
                vec![json!({
                    "type": "tool_result",
                    "tool_use_id": message["tool_call_id"],
                    "content": text_of(&message["content"], "a tool message")?,
                })],
            ),
            _ => ("user", user_to_messages(&message["content"])?),
        };
        // Anthropic alternates roles: a tool result and the user text after it are one turn
        match messages.last_mut() {
            Some(last) if last["role"] == role => {
                if let Some(content) = last["content"].as_array_mut() {
                    content.extend(blocks);
                }
            }
            _ => messages.push(json!({"role": role, "content": blocks})),
        }
    }
    let max_tokens = request
        .get("max_completion_tokens")
        .or_else(|| request.get("max_tokens"))
        .ok_or("max_tokens is needed: Anthropic models have no default")?;
    let mut out =
        json!({"model": request["model"], "messages": messages, "max_tokens": max_tokens});
    if !system.is_empty() {
        out["system"] = system.join("\n\n").into();
    }
    for key in ["temperature", "top_p", "stream"] {
        if let Some(v) = request.get(key) {
            out[key] = v.clone();
        }
    }
    match &request["stop"] {
        Value::Null => {}
        Value::String(s) => out["stop_sequences"] = json!([s]),
        list => out["stop_sequences"] = list.clone(),
    }
    if let Some(user) = request.get("user") {
        out["metadata"] = json!({"user_id": user});
    }
    chat_tools(request, &mut out)?;
    Ok(out)
}

/// Tools, the tool choice and `parallel_tool_calls` of a Chat Completions request, in the Messages API.
fn chat_tools(request: &Value, out: &mut Value) -> Result<(), String> {
    if let Some(tools) = request.get("tools") {
        out["tools"] = tools
            .as_array()
            .ok_or("tools is not a list")?
            .iter()
            .map(|t| {
                let f = &t["function"];
                if t["type"] != "function" {
                    return Err(format!("a {} tool", t["type"].as_str().unwrap_or("?")));
                }
                let mut tool = json!({
                    "name": f["name"],
                    "input_schema": f.get("parameters").cloned().unwrap_or(json!({"type": "object"})),
                });
                if let Some(d) = f.get("description") {
                    tool["description"] = d.clone();
                }
                Ok(tool)
            })
            .collect::<Result<Vec<_>, String>>()?
            .into();
    }
    let mut choice = match &request["tool_choice"] {
        Value::Null => None,
        Value::String(s) => Some(match s.as_str() {
            "auto" => json!({"type": "auto"}),
            "none" => json!({"type": "none"}),
            "required" => json!({"type": "any"}),
            other => return Err(format!("tool_choice {other}")),
        }),
        named => Some(json!({"type": "tool", "name": named["function"]["name"]})),
    };
    if request["parallel_tool_calls"] == false {
        choice.get_or_insert_with(|| json!({"type": "auto"}))["disable_parallel_tool_use"] =
            true.into();
    }
    if let Some(choice) = choice {
        out["tool_choice"] = choice;
    }
    Ok(())
}

fn assistant_to_messages(message: &Value) -> Result<Vec<Value>, String> {
    let mut blocks = Vec::new();
    let text = text_of(&message["content"], "an assistant message")?;
    if !text.is_empty() {
        blocks.push(json!({"type": "text", "text": text}));
    }
    for call in message["tool_calls"].as_array().into_iter().flatten() {
        let arguments = call["function"]["arguments"].as_str().unwrap_or("{}");
        let input: Value = serde_json::from_str(arguments)
            .map_err(|_| "a tool call's arguments are not JSON".to_string())?;
        blocks.push(json!({"type": "tool_use", "id": call["id"], "name": call["function"]["name"], "input": input}));
    }
    Ok(blocks)
}

fn user_to_messages(content: &Value) -> Result<Vec<Value>, String> {
    let Value::Array(parts) = content else {
        return Ok(vec![
            json!({"type": "text", "text": text_of(content, "a user message")?}),
        ]);
    };
    parts
        .iter()
        .map(|p| match p["type"].as_str() {
            Some("text") => Ok(json!({"type": "text", "text": p["text"]})),
            Some("image_url") => {
                let url = p["image_url"]["url"].as_str().unwrap_or_default();
                let source = match url
                    .strip_prefix("data:")
                    .and_then(|d| d.split_once(";base64,"))
                {
                    Some((media_type, data)) => {
                        json!({"type": "base64", "media_type": media_type, "data": data})
                    }
                    None => json!({"type": "url", "url": url}),
                };
                Ok(json!({"type": "image", "source": source}))
            }
            other => Err(format!(
                "a user {} part cannot go to an Anthropic model",
                other.unwrap_or("?")
            )),
        })
        .collect()
}

fn stop_to_finish(stop: &str) -> &'static str {
    match stop {
        "max_tokens" => "length",
        "tool_use" => "tool_calls",
        "refusal" => "content_filter",
        _ => "stop",
    }
}

fn finish_to_stop(finish: &str) -> &'static str {
    match finish {
        "length" => "max_tokens",
        "tool_calls" | "function_call" => "tool_use",
        "content_filter" => "refusal",
        _ => "end_turn",
    }
}

fn created() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// An error in the caller's API: what the gateway says when it cannot give the provider's answer.
#[must_use]
pub fn caller_error(direction: Direction, message: &str) -> Value {
    match direction {
        Direction::MessagesToChat => {
            json!({"type": "error", "error": {"type": "api_error", "message": message}})
        }
        Direction::ChatToMessages => {
            json!({"error": {"message": message, "type": "legionctl_gateway", "code": null}})
        }
    }
}

/// The provider's whole answer (JSON, success or error) in the caller's API.
///
/// # Errors
/// The answer is not one of the provider's API (no choice, a tool call whose arguments are not JSON): refused with
/// the reason, never changed (19-gateway).
pub fn response(direction: Direction, body: &Value) -> Result<Value, String> {
    Ok(match direction {
        Direction::MessagesToChat => {
            if let Some(error) = body.get("error") {
                return Ok(json!({"type": "error", "error": {
                    "type": error["type"].as_str().unwrap_or("api_error"),
                    "message": error["message"],
                }}));
            }
            let choice = &body["choices"][0];
            let message = &choice["message"];
            if !message.is_object() {
                return Err(format!("the provider's answer has no message: {body}"));
            }
            let mut content = Vec::new();
            if let Some(text) = message["content"].as_str().filter(|t| !t.is_empty()) {
                content.push(json!({"type": "text", "text": text}));
            }
            for call in message["tool_calls"].as_array().into_iter().flatten() {
                let arguments = call["function"]["arguments"]
                    .as_str()
                    .ok_or("a tool call has no arguments")?;
                let input = serde_json::from_str::<Value>(arguments).map_err(|e| {
                    format!("a tool call's arguments are not JSON ({e}): {arguments}")
                })?;
                content.push(json!({"type": "tool_use", "id": call["id"], "name": call["function"]["name"], "input": input}));
            }
            json!({
                "id": body["id"], "type": "message", "role": "assistant", "model": body["model"], "content": content,
                "stop_reason": finish_to_stop(choice["finish_reason"].as_str().unwrap_or("stop")),
                "stop_sequence": null,
                "usage": {
                    "input_tokens": body["usage"]["prompt_tokens"].as_i64().unwrap_or(0),
                    "output_tokens": body["usage"]["completion_tokens"].as_i64().unwrap_or(0),
                },
            })
        }
        Direction::ChatToMessages => {
            if body["type"] == "error" {
                return Ok(json!({"error": {
                    "message": body["error"]["message"],
                    "type": body["error"]["type"],
                    "code": null,
                }}));
            }
            let blocks = body["content"].as_array().cloned().unwrap_or_default();
            let text: String = blocks
                .iter()
                .filter(|b| b["type"] == "text")
                .filter_map(|b| b["text"].as_str())
                .collect();
            let calls: Vec<Value> = blocks
                .iter()
                .filter(|b| b["type"] == "tool_use")
                .map(|b| json!({"id": b["id"], "type": "function", "function": {"name": b["name"], "arguments": b["input"].to_string()}}))
                .collect();
            let mut message = json!({"role": "assistant", "content": text});
            if !calls.is_empty() {
                message["tool_calls"] = calls.into();
            }
            let (input, output) = (
                body["usage"]["input_tokens"].as_i64().unwrap_or(0),
                body["usage"]["output_tokens"].as_i64().unwrap_or(0),
            );
            json!({
                "id": body["id"], "object": "chat.completion", "created": created(), "model": body["model"],
                "choices": [{"index": 0, "message": message,
                    "finish_reason": stop_to_finish(body["stop_reason"].as_str().unwrap_or("end_turn"))}],
                "usage": {"prompt_tokens": input, "completion_tokens": output, "total_tokens": input + output},
            })
        }
    })
}

/// A provider's SSE stream, translated event by event as its bytes arrive.
pub struct Stream {
    direction: Direction,
    /// Bytes after the last complete event.
    pending: String,
    started: bool,
    done: bool,
    id: Value,
    model: Value,
    /// The block open in the caller's stream (`MessagesToChat`): its index and the `OpenAI` tool call it carries.
    open: Option<(usize, Option<u64>)>,
    blocks: usize,
    /// Anthropic block index -> `OpenAI` tool call index (`ChatToMessages`).
    calls: Vec<usize>,
    stop: String,
    tokens: (i64, i64),
    /// The `OpenAI` caller asked for the usage chunk.
    include_usage: bool,
}

impl Stream {
    #[must_use]
    pub fn new(direction: Direction, request: &Value) -> Self {
        Self {
            direction,
            pending: String::new(),
            started: false,
            done: false,
            id: Value::Null,
            model: request["model"].clone(),
            open: None,
            blocks: 0,
            calls: Vec::new(),
            stop: String::new(),
            tokens: (0, 0),
            include_usage: request["stream_options"]["include_usage"] == true,
        }
    }

    /// The caller's events for the provider's bytes so far.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.pending
            .push_str(&String::from_utf8_lossy(bytes).replace("\r\n", "\n"));
        let mut out = String::new();
        while let Some(end) = self.pending.find("\n\n") {
            let event: String = self.pending.drain(..end + 2).collect();
            let data: Vec<&str> = event
                .lines()
                .filter_map(|l| l.strip_prefix("data:"))
                .map(str::trim)
                .collect();
            let data = data.join("\n");
            if data.is_empty() {
                continue;
            }
            match self.direction {
                Direction::MessagesToChat => self.on_chat_chunk(&data, &mut out),
                Direction::ChatToMessages => self.on_messages_event(&data, &mut out),
            }
        }
        out.into_bytes()
    }

    /// The end of the provider's stream: one that ended before its last event is an error event, never a complete
    /// answer (a cut stream would read as a whole one).
    pub fn finish(&mut self) -> Vec<u8> {
        let mut out = String::new();
        if self.started && !self.done {
            self.fail(&mut out, "the provider's stream ended early");
        }
        out.into_bytes()
    }

    /// End the caller's stream with an error event; later events are not translated.
    fn fail(&mut self, out: &mut String, message: &str) {
        self.done = true;
        let error = caller_error(self.direction, message);
        match self.direction {
            Direction::MessagesToChat => Self::anthropic(out, &error),
            Direction::ChatToMessages => {
                let _ = write!(out, "data: {error}\n\n");
            }
        }
    }

    fn anthropic(out: &mut String, event: &Value) {
        let kind = event["type"].as_str().unwrap_or_default();
        let _ = write!(out, "event: {kind}\ndata: {event}\n\n");
    }

    fn close_block(&mut self, out: &mut String) {
        if let Some((index, _)) = self.open.take() {
            Self::anthropic(out, &json!({"type": "content_block_stop", "index": index}));
        }
    }

    fn open_block(&mut self, out: &mut String, block: &Value, call: Option<u64>) -> usize {
        self.close_block(out);
        let index = self.blocks;
        self.blocks += 1;
        Self::anthropic(
            out,
            &json!({"type": "content_block_start", "index": index, "content_block": block}),
        );
        self.open = Some((index, call));
        index
    }

    fn end_message(&mut self, out: &mut String) {
        if self.done || !self.started {
            return;
        }
        self.done = true;
        self.close_block(out);
        let stop = if self.stop.is_empty() {
            "end_turn"
        } else {
            &self.stop
        };
        Self::anthropic(
            out,
            &json!({"type": "message_delta", "delta": {"stop_reason": stop, "stop_sequence": null},
                "usage": {"input_tokens": self.tokens.0, "output_tokens": self.tokens.1}}),
        );
        Self::anthropic(out, &json!({"type": "message_stop"}));
    }

    /// One `OpenAI` chunk -> Anthropic events.
    fn on_chat_chunk(&mut self, data: &str, out: &mut String) {
        if self.done {
            return;
        }
        if data == "[DONE]" {
            self.end_message(out);
            return;
        }
        let chunk = match serde_json::from_str::<Value>(data) {
            Ok(chunk) => chunk,
            Err(e) => {
                self.started = true;
                return self.fail(
                    out,
                    &format!("the provider sent an event that is not JSON ({e}): {data}"),
                );
            }
        };
        if let Some(error) = chunk.get("error") {
            self.done = true;
            Self::anthropic(
                out,
                &json!({"type": "error", "error": {"type": error["type"].as_str().unwrap_or("api_error"), "message": error["message"]}}),
            );
            return;
        }
        if !self.started {
            self.started = true;
            Self::anthropic(
                out,
                &json!({"type": "message_start", "message": {"id": chunk["id"], "type": "message",
                    "role": "assistant", "content": [], "model": chunk["model"], "stop_reason": null,
                    "stop_sequence": null, "usage": {"input_tokens": 0, "output_tokens": 0}}}),
            );
        }
        if let Some(usage) = chunk.get("usage").filter(|u| u.is_object()) {
            self.tokens = (
                usage["prompt_tokens"].as_i64().unwrap_or(0),
                usage["completion_tokens"].as_i64().unwrap_or(0),
            );
        }
        let Some(choice) = chunk["choices"].get(0) else {
            return;
        };
        let delta = &choice["delta"];
        if let Some(text) = delta["content"].as_str().filter(|t| !t.is_empty()) {
            let index = match self.open {
                Some((index, None)) => index,
                _ => self.open_block(out, &json!({"type": "text", "text": ""}), None),
            };
            Self::anthropic(
                out,
                &json!({"type": "content_block_delta", "index": index, "delta": {"type": "text_delta", "text": text}}),
            );
        }
        for call in delta["tool_calls"].as_array().into_iter().flatten() {
            let k = call["index"].as_u64().unwrap_or(0);
            let index = match self.open {
                Some((index, Some(open))) if open == k => index,
                _ => self.open_block(
                    out,
                    &json!({"type": "tool_use", "id": call["id"], "name": call["function"]["name"], "input": {}}),
                    Some(k),
                ),
            };
            if let Some(part) = call["function"]["arguments"]
                .as_str()
                .filter(|a| !a.is_empty())
            {
                Self::anthropic(
                    out,
                    &json!({"type": "content_block_delta", "index": index, "delta": {"type": "input_json_delta", "partial_json": part}}),
                );
            }
        }
        if let Some(finish) = choice["finish_reason"].as_str() {
            finish_to_stop(finish).clone_into(&mut self.stop);
        }
    }

    fn chunk(&self, delta: &Value, finish: Option<&str>) -> String {
        let chunk = json!({"id": self.id, "object": "chat.completion.chunk", "created": created(),
            "model": self.model, "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]});
        format!("data: {chunk}\n\n")
    }

    /// One Anthropic event -> `OpenAI` chunks.
    fn on_messages_event(&mut self, data: &str, out: &mut String) {
        if self.done {
            return;
        }
        let event = match serde_json::from_str::<Value>(data) {
            Ok(event) => event,
            Err(e) => {
                self.started = true;
                return self.fail(
                    out,
                    &format!("the provider sent an event that is not JSON ({e}): {data}"),
                );
            }
        };
        match event["type"].as_str() {
            Some("message_start") => {
                self.started = true;
                let message = &event["message"];
                self.id = message["id"].clone();
                if !message["model"].is_null() {
                    self.model = message["model"].clone();
                }
                self.tokens.0 = message["usage"]["input_tokens"].as_i64().unwrap_or(0);
                out.push_str(&self.chunk(&json!({"role": "assistant", "content": ""}), None));
            }
            Some("content_block_start") if event["content_block"]["type"] == "tool_use" => {
                let index = usize::try_from(event["index"].as_u64().unwrap_or(0)).unwrap_or(0);
                let k = self.calls.iter().filter(|c| **c != usize::MAX).count();
                if self.calls.len() <= index {
                    self.calls.resize(index + 1, usize::MAX);
                }
                self.calls[index] = k;
                let block = &event["content_block"];
                out.push_str(&self.chunk(
                    &json!({"tool_calls": [{"index": k, "id": block["id"], "type": "function",
                        "function": {"name": block["name"], "arguments": ""}}]}),
                    None,
                ));
            }
            Some("content_block_delta") => {
                let delta = &event["delta"];
                match delta["type"].as_str() {
                    Some("text_delta") => {
                        out.push_str(&self.chunk(&json!({"content": delta["text"]}), None));
                    }
                    Some("input_json_delta") => {
                        let index =
                            usize::try_from(event["index"].as_u64().unwrap_or(0)).unwrap_or(0);
                        let k = self.calls.get(index).copied().unwrap_or(0);
                        out.push_str(&self.chunk(
                            &json!({"tool_calls": [{"index": k, "function": {"arguments": delta["partial_json"]}}]}),
                            None,
                        ));
                    }
                    // thinking is never asked for by a translated request
                    _ => {}
                }
            }
            Some("message_delta") => {
                if let Some(stop) = event["delta"]["stop_reason"].as_str() {
                    stop.clone_into(&mut self.stop);
                }
                let usage = &event["usage"];
                self.tokens.0 = self
                    .tokens
                    .0
                    .max(usage["input_tokens"].as_i64().unwrap_or(0));
                self.tokens.1 = usage["output_tokens"].as_i64().unwrap_or(self.tokens.1);
            }
            Some("message_stop") => {
                out.push_str(&self.chunk(&json!({}), Some(stop_to_finish(&self.stop))));
                if self.include_usage {
                    let (input, output) = self.tokens;
                    let chunk = json!({"id": self.id, "object": "chat.completion.chunk", "created": created(),
                        "model": self.model, "choices": [],
                        "usage": {"prompt_tokens": input, "completion_tokens": output, "total_tokens": input + output}});
                    let _ = write!(out, "data: {chunk}\n\n");
                }
                out.push_str("data: [DONE]\n\n");
                self.done = true;
            }
            Some("error") => {
                self.done = true;
                let error = json!({"error": {"message": event["error"]["message"], "type": event["error"]["type"], "code": null}});
                let _ = write!(out, "data: {error}\n\n");
            }
            _ => {}
        }
    }
}
