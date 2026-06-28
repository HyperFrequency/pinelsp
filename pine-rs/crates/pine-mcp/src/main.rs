//! `pine-mcp` — Model Context Protocol server for Pine Script v6.
//!
//! Speaks the MCP stdio transport (newline-delimited JSON-RPC 2.0) directly —
//! no SDK dependency — exposing four tools that wrap `pine-check` and the
//! builtins database:
//!   - `pine_validate`      lint a snippet
//!   - `pine_lookup`        builtin function/variable/constant docs
//!   - `pine_list_functions` list builtin functions (optionally by namespace)
//!   - `pine_format`        format a snippet

use std::io::{BufRead, Write};

use pine_core::{Document, builtins};
use serde_json::{Value, json};

fn main() {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(req) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        let id = req.get("id").cloned();

        // Notifications (no id / notifications/*) get no response.
        let result = match method {
            "initialize" => Some(json!({
                "protocolVersion": "2024-11-05",
                "serverInfo": { "name": "pine-script", "version": env!("CARGO_PKG_VERSION") },
                "capabilities": { "tools": {} }
            })),
            "tools/list" => Some(json!({ "tools": tool_specs() })),
            "tools/call" => Some(handle_call(req.get("params"))),
            "ping" => Some(json!({})),
            _ => None,
        };

        if let (Some(id), Some(result)) = (id, result) {
            let msg = json!({ "jsonrpc": "2.0", "id": id, "result": result });
            let _ = writeln!(out, "{}", serde_json::to_string(&msg).unwrap());
            let _ = out.flush();
        }
    }
}

fn tool_specs() -> Value {
    json!([
        {
            "name": "pine_validate",
            "description": "Lint Pine Script v6 source; returns diagnostics.",
            "inputSchema": { "type": "object", "properties": { "code": { "type": "string" } }, "required": ["code"] }
        },
        {
            "name": "pine_lookup",
            "description": "Look up a Pine builtin function/variable/constant by name.",
            "inputSchema": { "type": "object", "properties": { "name": { "type": "string" } }, "required": ["name"] }
        },
        {
            "name": "pine_list_functions",
            "description": "List builtin function names, optionally filtered by namespace (e.g. 'ta').",
            "inputSchema": { "type": "object", "properties": { "namespace": { "type": "string" } } }
        },
        {
            "name": "pine_format",
            "description": "Format Pine Script v6 source.",
            "inputSchema": { "type": "object", "properties": { "code": { "type": "string" } }, "required": ["code"] }
        }
    ])
}

fn handle_call(params: Option<&Value>) -> Value {
    let name = params
        .and_then(|p| p.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let args = params.and_then(|p| p.get("arguments"));
    let arg_str = |key: &str| {
        args.and_then(|a| a.get(key))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };

    let text = match name {
        "pine_validate" => validate(&arg_str("code")),
        "pine_lookup" => lookup(&arg_str("name")),
        "pine_list_functions" => {
            let ns = args
                .and_then(|a| a.get("namespace"))
                .and_then(Value::as_str);
            list_functions(ns)
        }
        "pine_format" => format_code(&arg_str("code")),
        other => {
            return json!({ "content": [text_block(format!("unknown tool: {other}"))], "isError": true });
        }
    };
    json!({ "content": [text_block(text)] })
}

fn text_block(s: String) -> Value {
    json!({ "type": "text", "text": s })
}

fn validate(code: &str) -> String {
    let Some(doc) = Document::parse(code) else {
        return "Failed to parse.".to_string();
    };
    let diags = pine_check::analyze(&doc);
    if diags.is_empty() {
        return "No issues found.".to_string();
    }
    diags
        .iter()
        .map(|d| {
            let (line, col) = doc.position_at(d.start_byte);
            format!(
                "{}:{}: {:?}: {} [{}]",
                line + 1,
                col + 1,
                d.severity,
                d.message,
                d.code
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn lookup(name: &str) -> String {
    if let Some(f) = builtins::function(name) {
        let head = if f.syntax.is_empty() {
            &f.name
        } else {
            &f.syntax
        };
        return format!("{head}\n\n{}\n\nreturns: {}", f.description, f.returns);
    }
    if let Some(v) = builtins::variable(name) {
        return format!("{}: {}\n\n{}", v.name, v.ty, v.description);
    }
    if let Some(c) = builtins::constant(name) {
        return format!("{}: {}", c.name, c.ty);
    }
    format!("`{name}` not found")
}

fn list_functions(namespace: Option<&str>) -> String {
    let mut names: Vec<&str> = builtins::FUNCTIONS
        .iter()
        .filter(|f| match namespace {
            Some(ns) => f.namespace.as_deref() == Some(ns),
            None => true,
        })
        .map(|f| f.name.as_str())
        .collect();
    names.sort_unstable();
    if names.is_empty() {
        "(none)".to_string()
    } else {
        names.join("\n")
    }
}

fn format_code(code: &str) -> String {
    let mut lines: Vec<String> = code.lines().map(|l| l.trim_end().to_string()).collect();
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    if lines.is_empty() {
        return String::new();
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}
