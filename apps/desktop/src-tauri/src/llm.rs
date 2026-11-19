//! LLM-backed function explanation (Ollama local, Gemini API).
//!
//! API keys stay on the backend; the frontend only sends an [`ExplainContext`].

use std::time::Duration;

pub use lcw_desktop_api::ExplainContext;
use lcw_desktop_api::{explain_prompt, EXPLAIN_SYSTEM_PROMPT as SYSTEM_PROMPT};

const TIMEOUT_SECS: u64 = 60;

/// Explain `context` using `provider` (`ollama` or `gemini`).
pub fn explain_function(context: ExplainContext, provider: &str) -> Result<String, String> {
    let prompt = explain_prompt(&context);
    match provider {
        "ollama" => explain_ollama(&prompt),
        "gemini" => explain_gemini(&prompt),
        other => Err(format!(
            "unknown provider: {other} (expected ollama or gemini)"
        )),
    }
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
    use lcw_desktop_api::ExplainMetrics;

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
        let p = explain_prompt(&sample_context());
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
