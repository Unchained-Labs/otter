//! An MCP server exposing task complexity scoring over stdio.
//!
//! Speaks JSON-RPC 2.0 framed as line-delimited JSON on stdin/stdout, which is
//! the stdio transport MCP clients use. Implemented directly rather than via an
//! SDK: the surface is three methods and two tools, and a hand-rolled version
//! keeps this crate dependency-light enough to lift into its own repository.
//!
//! Register it with an MCP client as:
//!
//! ```json
//! { "command": "otter-complexity-mcp", "args": [] }
//! ```
//!
//! Tools:
//! - `evaluate_complexity` — score one prompt.
//! - `rank_tasks` — score several prompts and return them in execution order.

use std::io::{self, BufRead, Write};

use otter_complexity::{assess_with_context, TaskAssessment, TaskContext};
use serde_json::{json, Value};

const PROTOCOL_VERSION: &str = "2024-11-05";

fn main() {
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let request: Value = match serde_json::from_str(trimmed) {
            Ok(value) => value,
            Err(error) => {
                // -32700 is the JSON-RPC parse-error code. No id is available.
                write_message(
                    &mut stdout,
                    &error_response(Value::Null, -32700, &error.to_string()),
                );
                continue;
            }
        };

        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        let params = request.get("params").cloned().unwrap_or(json!({}));

        // Notifications carry no id and must not be answered.
        let is_notification = request.get("id").is_none();
        let response = match method {
            "initialize" => Some(success(id, initialize_result())),
            "tools/list" => Some(success(id, tools_list())),
            "tools/call" => Some(match handle_tool_call(&params) {
                Ok(result) => success(id, result),
                Err(message) => error_response(id, -32602, &message),
            }),
            "ping" => Some(success(id, json!({}))),
            _ if is_notification => None,
            _ => Some(error_response(
                id,
                -32601,
                &format!("unknown method: {method}"),
            )),
        };

        if let Some(response) = response {
            write_message(&mut stdout, &response);
        }
    }
}

fn write_message(stdout: &mut io::Stdout, message: &Value) {
    let _ = writeln!(stdout, "{message}");
    let _ = stdout.flush();
}

fn success(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error_response(id: Value, code: i32, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "otter-complexity", "version": env!("CARGO_PKG_VERSION") }
    })
}

fn tools_list() -> Value {
    json!({
        "tools": [
            {
                "name": "evaluate_complexity",
                "description":
                    "Score a natural-language build task for complexity (1-10), size (1-10) and \
                     scheduling intensity (0-100). Deterministic: the same prompt always returns \
                     the same score. Returns the signals behind the score so it can be explained.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "prompt": {
                            "type": "string",
                            "description": "The task description to score."
                        },
                        "dependency_count": {
                            "type": "integer",
                            "minimum": 0,
                            "description": "How many other tasks this one waits on."
                        },
                        "scoped_to_project_path": {
                            "type": "boolean",
                            "description": "True when the task is confined to a known subpath."
                        }
                    },
                    "required": ["prompt"]
                }
            },
            {
                "name": "rank_tasks",
                "description":
                    "Score several task prompts and return them in recommended execution order, \
                     shortest and simplest first. Use to plan a backlog.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "prompts": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Task descriptions to rank."
                        }
                    },
                    "required": ["prompts"]
                }
            }
        ]
    })
}

fn handle_tool_call(params: &Value) -> Result<Value, String> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing tool name".to_string())?;
    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

    match name {
        "evaluate_complexity" => {
            let prompt = arguments
                .get("prompt")
                .and_then(Value::as_str)
                .ok_or_else(|| "`prompt` is required and must be a string".to_string())?;
            let context = TaskContext {
                dependency_count: arguments
                    .get("dependency_count")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize,
                scoped_to_project_path: arguments
                    .get("scoped_to_project_path")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            };
            let assessment = assess_with_context(prompt, &context);
            Ok(tool_result(&summarize(prompt, &assessment), &assessment))
        }
        "rank_tasks" => {
            let prompts = arguments
                .get("prompts")
                .and_then(Value::as_array)
                .ok_or_else(|| "`prompts` is required and must be an array".to_string())?;

            let mut ranked: Vec<(String, TaskAssessment)> = prompts
                .iter()
                .filter_map(Value::as_str)
                .map(|prompt| {
                    (
                        prompt.to_string(),
                        assess_with_context(prompt, &TaskContext::default()),
                    )
                })
                .collect();
            ranked.sort_by_key(|(_, assessment)| assessment.intensity);

            let summary = ranked
                .iter()
                .enumerate()
                .map(|(index, (prompt, assessment))| {
                    format!(
                        "{}. [{} · intensity {} · ~{}m] {}",
                        index + 1,
                        assessment.band.as_str(),
                        assessment.intensity,
                        assessment.estimated_minutes,
                        prompt
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");

            let structured = json!({
                "order": ranked
                    .iter()
                    .map(|(prompt, assessment)| json!({
                        "prompt": prompt,
                        "assessment": assessment,
                    }))
                    .collect::<Vec<_>>()
            });
            Ok(tool_result(&summary, &structured))
        }
        other => Err(format!("unknown tool: {other}")),
    }
}

/// MCP tool results carry human-readable `content` plus machine-readable
/// `structuredContent`; clients that understand only one still work.
fn tool_result(text: &str, structured: &impl serde::Serialize) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": serde_json::to_value(structured).unwrap_or(Value::Null)
    })
}

fn summarize(prompt: &str, assessment: &TaskAssessment) -> String {
    let signals = assessment
        .signals
        .iter()
        .take(5)
        .map(|signal| signal.detail.clone())
        .collect::<Vec<_>>()
        .join(", ");

    format!(
        "{}\n\ncomplexity {}/10 · size {}/10 · intensity {}/100 ({})\n\
         estimate ~{} min · confidence {:.0}%\nsignals: {}",
        prompt.trim(),
        assessment.complexity,
        assessment.size,
        assessment.intensity,
        assessment.band.as_str(),
        assessment.estimated_minutes,
        assessment.confidence * 100.0,
        if signals.is_empty() {
            "none".to_string()
        } else {
            signals
        }
    )
}
