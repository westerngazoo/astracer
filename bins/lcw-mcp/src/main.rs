//! `lcw-mcp` — a Model Context Protocol server over stdio.
//!
//! Newline-delimited JSON-RPC 2.0 on stdin/stdout (the MCP stdio transport);
//! logs go to stderr only, so stdout stays a clean protocol stream. Loaded
//! graphs are kept per repository for the life of the process, so repeated
//! questions about one repo are answered from memory.

mod server;

use std::io::{BufRead, Write};

fn main() -> anyhow::Result<()> {
    lcw_telemetry::init(&std::env::var("LCW_MCP_LOG").unwrap_or_else(|_| "warn".into()));
    let mut server = server::Server::new();
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = server.handle_line(&line) {
            writeln!(stdout, "{reply}")?;
            stdout.flush()?;
        }
    }
    Ok(())
}
