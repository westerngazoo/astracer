//! LLM-backed function explanation (Ollama local, Gemini API).
//!
//! API keys stay on the backend; the frontend only sends an [`ExplainContext`].

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Metrics forwarded from the node card.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplainMetrics {
    pub cyclomatic: u32,
    pub lines_of_code: u32,
    pub parameters: u32,
    pub max_nesting: u32,
    pub allocations: u32,
}

/// Everything the model needs to explain one function.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplainContext {
    pub qualified_name: String,
    pub kind: String,
    pub file: String,
    pub start_line: u32,
    pub end_line: u32,
    pub metrics: ExplainMetrics,
    pub fan_in: usize,
    pub fan_out: usize,
    /// Top caller qualified names (up to 5).
    pub callers: Vec<String>,
    /// Top callee qualified names (up to 5).
    pub callees: Vec<String>,
    pub source: String,
    /// Optional one-level call-tree summary.
    #[serde(default)]
    pub call_tree_summary: Option<String>,
}

const SYSTEM_PROMPT: &str = "You are a code-reading assistant for a call-graph explorer. \
Explain what the given function does in plain language. \
Describe its role in the call graph: why its callers matter and what its callees imply. \
Be concise (3–6 short paragraphs). Use bullet lists for callers/callees when helpful. \
Do not invent behavior not visible in the source.";

const TIMEOUT_SECS: u64 = 60;

/// Explain `context` using `provider` (`ollama` or `gemini`).
pub fn explain_function(context: ExplainContext, provider: &str) -> Result<String, String> {
    let prompt = build_prompt(&context);
    match provider {
        "ollama" => explain_ollama(&prompt),
        "gemini" => explain_gemini(&prompt),
        other => Err(format!("unknown provider: {other} (expected ollama or gemini)")),
    }
}

fn build_prompt(ctx: &ExplainContext) -> String {
    let mut s = format!(
        "Function: {}\nKind: {}\nLocation: {}:{}-{}\n\
         Fan-in: {} · Fan-out: {}\n\
         Metrics: cyclomatic={}, LOC={}, params={}, max_nesting={}, allocations={}\n",
        ctx.qualified_name,
        ctx.kind,
        ctx.file,
        ctx.start_line,
        ctx.end_line,
        ctx.fan_in,
        ctx.fan_out,
        ctx.metrics.cyclomatic,
        ctx.metrics.lines_of_code,
        ctx.metrics.parameters,
        ctx.metrics.max_nesting,
        ctx.metrics.allocations,
    );
    if !ctx.callers.is_empty() {
        s.push_str("\nCallers:\n");
        for c in &ctx.callers {
            s.push_str(&format!("- {c}\n"));
        }
    }
    if !ctx.callees.is_empty() {
        s.push_str("\nCallees:\n");
        for c in &ctx.callees {
            s.push_str(&format!("- {c}\n"));
        }
    }
    if let Some(tree) = &ctx.call_tree_summary {
        s.push_str(&format!("\nCall tree (one level):\n{tree}\n"));
    }
    s.push_str("\nSource:\n```\n");
    s.push_str(&ctx.source);
    s.push_str("\n```\n");
    s
}

fn http_client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("HTTP client: {e}"))
}

fn explain_ollama(prompt: &str) -> Result<String, String> {
    let model = std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| "qwen2.5-coder:7b".to_string());
    let client = http_client()?;
    let body = serde_json::json!({
        "model": model,
        "prompt": format!("{SYSTEM_PROMPT}\n\n{prompt}"),
        "stream": false,
    });
    let resp = client
        .post("http://localhost:11434/api/generate")
        .json(&body)
        .send()
        .map_err(|e| {
            if e.is_connect() || e.is_timeout() {
                "Ollama not reachable at localhost:11434".to_string()
            } else {
                format!("Ollama request failed: {e}")
            }
        })?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().unwrap_or_default();
        return Err(format!("Ollama error ({status}): {text}"));
    }
    let json: serde_json::Value = resp.json().map_err(|e| format!("Ollama response: {e}"))?;
    json["response"]
        .as_str()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "Ollama returned an empty response".to_string())
}

fn explain_gemini(prompt: &str) -> Result<String, String> {
    let key = std::env::var("GEMINI_API_KEY").map_err(|_| {
        "GEMINI_API_KEY is not set — export it in the environment before using Gemini".to_string()
    })?;
    let client = http_client()?;
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-flash:generateContent?key={key}"
    );
    let body = serde_json::json!({
        "systemInstruction": {
            "parts": [{ "text": SYSTEM_PROMPT }]
        },
        "contents": [{
            "parts": [{ "text": prompt }]
        }],
    });
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .map_err(|e| format!("Gemini request failed: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().unwrap_or_default();
        return Err(format!("Gemini error ({status}): {text}"));
    }
    let json: serde_json::Value = resp.json().map_err(|e| format!("Gemini response: {e}"))?;
    json["candidates"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|c| c["content"]["parts"].as_array())
        .and_then(|p| p.first())
        .and_then(|p| p["text"].as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "Gemini returned an empty response".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_context() -> ExplainContext {
        ExplainContext {
            qualified_name: "app::run".into(),
            kind: "function".into(),
            file: "src/main.rs".into(),
            start_line: 10,
            end_line: 20,
            metrics: ExplainMetrics {
                cyclomatic: 3,
                lines_of_code: 11,
                parameters: 0,
                max_nesting: 2,
                allocations: 1,
            },
            fan_in: 1,
            fan_out: 2,
            callers: vec!["app::main".into()],
            callees: vec!["app::parse".into(), "app::render".into()],
            source: "fn run() { parse(); render(); }".into(),
            call_tree_summary: Some("→ parse, render".into()),
        }
    }

    #[test]
    fn prompt_includes_context_fields() {
        let p = build_prompt(&sample_context());
        assert!(p.contains("app::run"));
        assert!(p.contains("app::main"));
        assert!(p.contains("app::parse"));
        assert!(p.contains("fn run()"));
        assert!(p.contains("→ parse, render"));
    }

    #[test]
    fn unknown_provider_errors() {
        let err = explain_function(sample_context(), "openai").unwrap_err();
        assert!(err.contains("unknown provider"));
    }
}
