//! Minimal MCP server: newline-delimited JSON-RPC 2.0 over stdio.
//! stdout carries protocol messages only; all logging goes to stderr.

use std::io::{BufRead, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};

use anyhow::Result;
use serde_json::{json, Value};

use crate::tools::{tool_definitions, Engine};

const SUPPORTED: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "BlastCode gives you a structural index of the codebase. \
Prefer it over reading whole files or grepping: start with get_workspace_map, outline files with get_file_skeleton, \
find symbols with search_symbols, and use trace_symbol for callers/callees. \
Workspace changes since your previous call are reported automatically at the top of tool results; \
trust them instead of re-reading files. Use get_symbol_source to read a single function rather than a whole file. \
Before changing a function signature, run get_impact_radius with new_source; after editing, run it again without new_source. \
Edges are tagged exact/probable/heuristic; only 'breaking' findings are provable.";

pub fn serve(mut engine: Engine) -> Result<()> {
    engine.spawn_watcher();
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                write_msg(
                    &stdout,
                    &json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":format!("parse error: {e}")}}),
                )?;
                continue;
            }
        };
        let out = if let Some(arr) = msg.as_array() {
            let rs: Vec<Value> = arr.iter().filter_map(|m| handle(&mut engine, m)).collect();
            if rs.is_empty() {
                None
            } else {
                Some(Value::Array(rs))
            }
        } else {
            handle(&mut engine, &msg)
        };
        if let Some(o) = out {
            write_msg(&stdout, &o)?;
        }
    }
    Ok(())
}

fn write_msg(stdout: &std::io::Stdout, v: &Value) -> Result<()> {
    let mut lock = stdout.lock();
    writeln!(lock, "{}", serde_json::to_string(v)?)?;
    lock.flush()?;
    Ok(())
}

/// Handle one JSON-RPC message. Returns `None` for notifications and client responses.
pub fn handle(engine: &mut Engine, msg: &Value) -> Option<Value> {
    let method = msg.get("method").and_then(|m| m.as_str())?;
    let id = msg.get("id").cloned();
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result: Result<Value, (i64, String)> = match method {
        "initialize" => Ok(init_result(&params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_definitions() })),
        "tools/call" => call_tool(engine, &params),
        m if m.starts_with("notifications/") => return None,
        _ => Err((-32601, format!("method not found: {method}"))),
    };
    let id = id?;
    Some(match result {
        Ok(r) => json!({"jsonrpc":"2.0","id":id,"result":r}),
        Err((code, message)) => json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}}),
    })
}

fn init_result(params: &Value) -> Value {
    let requested = params.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or("");
    let version = if SUPPORTED.contains(&requested) { requested } else { SUPPORTED[0] };
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": "blastcode", "version": env!("CARGO_PKG_VERSION") },
        "instructions": INSTRUCTIONS
    })
}

fn call_tool(engine: &mut Engine, params: &Value) -> Result<Value, (i64, String)> {
    let name = params
        .get("name")
        .and_then(|n| n.as_str())
        .ok_or((-32602, "missing tool name".to_string()))?;
    if !Engine::has_tool(name) {
        return Err((-32602, format!("unknown tool: {name}")));
    }
    let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
    let outcome = catch_unwind(AssertUnwindSafe(|| engine.call_with_digest(name, &args)));
    let (digest, text, is_error) = match outcome {
        Ok(Ok((d, t))) => (d, t, false),
        Ok(Err(e)) => (None, format!("{e:#}"), true),
        Err(_) => (None, "internal error while running the tool".to_string(), true),
    };
    // The workspace-change digest travels as its own content block so the tool
    // result itself stays clean (pure JSON for most tools).
    let mut content = Vec::new();
    if let Some(d) = digest {
        content.push(json!({ "type": "text", "text": d }));
    }
    content.push(json!({ "type": "text", "text": text }));
    Ok(json!({ "content": content, "isError": is_error }))
}
