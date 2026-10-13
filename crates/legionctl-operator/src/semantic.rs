//! The gateway's semantic cache (note 19, G2): a prompt's embedding from the configured `OpenAI`-API account, so a
//! reworded prompt can replay a kept answer when the caller asks for it (`x-legionctl-cache: semantic`).

use anyhow::{Context, Result};
use legionctl_core::config::{GatewayConfig, SemanticConfig};
use legionctl_core::db::GatewayUsage;
use serde_json::{Value, json};

/// The prompt as text: system, instructions and each message's role and text, in order.
pub fn prompt_text(request: &Value) -> String {
    fn texts(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::String(s) => out.push(s.clone()),
            Value::Array(items) => items.iter().for_each(|i| texts(i, out)),
            Value::Object(o) => {
                for key in ["role", "content", "text"] {
                    if let Some(inner) = o.get(key) {
                        texts(inner, out);
                    }
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    for key in ["system", "instructions", "messages", "input"] {
        texts(&request[key], &mut out);
    }
    out.join("\n")
}

/// The embedding of `text` from the semantic account, and the usage row of that call.
///
/// # Errors
/// The account does not answer, or answers without an embedding.
pub async fn embed(
    http: &reqwest::Client,
    config: &GatewayConfig,
    semantic: &SemanticConfig,
    text: &str,
) -> Result<(Vec<f64>, GatewayUsage)> {
    let account = config
        .accounts
        .get(&semantic.account)
        .context("no semantic account")?;
    let mut request = http
        .post(format!(
            "{}/v1/embeddings",
            account.base_url.trim_end_matches('/')
        ))
        .json(&json!({"model": semantic.model, "input": text}));
    if account.keyring {
        request = request.bearer_auth(crate::gateway::key(&semantic.account).await?);
    }
    let response = request.send().await?;
    let status = response.status().as_u16();
    let body: Value = response.json().await?;
    anyhow::ensure!(
        (200..300).contains(&status),
        "embeddings from {}: {status} {body}",
        semantic.account
    );
    let vector: Vec<f64> = body["data"][0]["embedding"]
        .as_array()
        .context("the answer has no embedding")?
        .iter()
        .filter_map(Value::as_f64)
        .collect();
    let tokens_in = body["usage"]["prompt_tokens"].as_i64().unwrap_or(0);
    let row = GatewayUsage {
        account: semantic.account.clone(),
        model: semantic.model.clone(),
        tokens_in,
        status,
        cost_usd: config
            .prices
            .get(&semantic.model)
            .map_or(0.0, |p| p.cost(tokens_in, 0)),
        ..GatewayUsage::default()
    };
    Ok((vector, row))
}
