//! The LLM prompt for "Explain this function", shared by every host so the
//! Tauri shell (which calls Ollama/Gemini itself) and the VS Code extension
//! (which gets the prompt from `lcw explain-prompt`) ask the same question.

use crate::ExplainContext;

/// System instruction sent alongside [`explain_prompt`].
pub const EXPLAIN_SYSTEM_PROMPT: &str =
    "You are a code-reading assistant for a call-graph explorer. \
Explain what the given function does in plain language. \
Describe its role in the call graph: why its callers matter and what its callees imply. \
Be concise (3–6 short paragraphs). Use bullet lists for callers/callees when helpful. \
Do not invent behavior not visible in the source.";

/// The user prompt for `ctx`: location, metrics, callers/callees and source.
pub fn explain_prompt(ctx: &ExplainContext) -> String {
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
    push_list(&mut s, "Callers", &ctx.callers);
    push_list(&mut s, "Callees", &ctx.callees);
    if let Some(tree) = &ctx.call_tree_summary {
        s.push_str(&format!("\nCall tree (one level):\n{tree}\n"));
    }
    s.push_str("\nSource:\n```\n");
    s.push_str(&ctx.source);
    s.push_str("\n```\n");
    s
}

fn push_list(s: &mut String, title: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    s.push_str(&format!("\n{title}:\n"));
    for c in items {
        s.push_str(&format!("- {c}\n"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ExplainMetrics;

    #[test]
    fn prompt_includes_context_fields() {
        let ctx = ExplainContext {
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
        };
        let p = explain_prompt(&ctx);
        assert!(p.contains("app::run"));
        assert!(p.contains("Callers:\n- app::main"));
        assert!(p.contains("app::parse"));
        assert!(p.contains("fn run()"));
        assert!(p.contains("→ parse, render"));
        assert!(p.contains("src/main.rs:10-20"));
    }
}
