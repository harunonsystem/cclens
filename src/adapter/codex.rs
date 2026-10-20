//! Codex rollout adapter. Explicit response usage wins over cumulative snapshots.
use super::multi as common;
use crate::multi::{Batch, Session, Tokens};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

pub fn parse(text: &str, path: &str) -> Batch {
    let mut batch = Batch::default();
    let rows = common::lines(text, &mut batch);
    let meta = rows
        .iter()
        .find(|(_, v)| v["type"] == "session_meta")
        .map(|(_, v)| &v["payload"]);
    let id = meta
        .and_then(|v| common::string(&v["id"]))
        .unwrap_or_else(|| path.into());
    batch.sessions.push(Session {
        id: id.clone(),
        project: meta.and_then(|v| common::string(&v["cwd"])),
        parent_id: meta.and_then(|v| common::string(&v["parent_thread_id"])),
        started_ms: meta.and_then(|v| common::timestamp(&v["timestamp"])),
    });
    let explicit_start = rows
        .iter()
        .find(|(_, v)| v["type"] == "token_usage_record")
        .map(|(line, v)| (*line, common::timestamp(&v["timestamp"])));
    let lifecycle_calls: HashSet<_> = rows
        .iter()
        .filter(|(_, v)| {
            v["type"] == "event_msg"
                && v["payload"]["type"] == "item_completed"
                && v["payload"]["item"]["type"] == "CommandExecution"
        })
        .filter_map(|(_, v)| common::string(&v["payload"]["item"]["id"]))
        .collect();
    let mut model = None;
    let mut models = HashMap::new();
    let mut seen = HashSet::new();
    let mut cumulative: Option<Tokens> = None;
    let mut calls: HashMap<String, (String, Value)> = HashMap::new();
    for (line, v) in rows {
        let p = &v["payload"];
        let ts = common::timestamp(&v["timestamp"]);
        match v["type"].as_str() {
            Some("session_meta") => {}
            Some("turn_context") => {
                model = common::string(&p["model"]);
                if let Some(turn) = common::string(&p["turn_id"]) {
                    models.insert(turn, model.clone());
                }
            }
            Some("token_usage_record") => {
                let Some(response) = common::string(&p["response_id"]) else {
                    batch
                        .warnings
                        .push(format!("usage without response identity at line {line}"));
                    continue;
                };
                let key = format!("response:{response}");
                if !seen.insert(key.clone()) {
                    continue;
                }
                let session_id = common::string(&p["thread_id"]).unwrap_or_else(|| id.clone());
                if !batch.sessions.iter().any(|s| s.id == session_id) {
                    batch.sessions.push(Session {
                        id: session_id.clone(),
                        parent_id: Some(id.clone()),
                        ..Default::default()
                    });
                }
                let mut e = common::event(key, &session_id, "usage", ts, path, line);
                e.tokens = tokens(&p["usage"]);
                e.model = common::string(&p["model"])
                    .or_else(|| {
                        p["turn_id"]
                            .as_str()
                            .and_then(|t| models.get(t))
                            .cloned()
                            .flatten()
                    })
                    .or_else(|| (session_id == id).then(|| model.clone()).flatten());
                if e.tokens == Tokens::default() {
                    e.measurement = "unavailable".into();
                }
                batch.events.push(e);
            }
            Some("event_msg") if p["type"] == "token_count" => {
                if explicit_start.is_some_and(|(first_line, first_ts)| {
                    line >= first_line || ts.zip(first_ts).is_none_or(|(old, new)| old >= new)
                }) {
                    continue;
                }
                let current = tokens(&p["info"]["total_token_usage"]);
                if current == Tokens::default() {
                    continue;
                }
                let last = tokens(&p["info"]["last_token_usage"]);
                if cumulative.as_ref() == Some(&current) {
                    continue;
                }
                let delta = if let Some(prev) = &cumulative {
                    if current.total.zip(prev.total).is_some_and(|(a, b)| a < b) {
                        batch
                            .warnings
                            .push(format!("cumulative usage reset at line {line}"));
                        if last == Tokens::default() {
                            current.clone()
                        } else {
                            last
                        }
                    } else {
                        subtract(&current, prev)
                    }
                } else if last != Tokens::default() {
                    if last != current {
                        batch.warnings.push(format!("unobserved cumulative prefix at line {line}; counted last response only"));
                    }
                    last
                } else if batch.sessions[0].parent_id.is_some() {
                    batch.warnings.push(format!(
                        "fork cumulative baseline unavailable at line {line}; excluded"
                    ));
                    cumulative = Some(current);
                    continue;
                } else {
                    current.clone()
                };
                // Copied histories retain the original timestamp and cumulative boundary.
                let key = format!(
                    "cumulative:{ts:?}:{}",
                    serde_json::to_string(&current).unwrap()
                );
                cumulative = Some(current);
                let mut e = common::event(key, &id, "usage", ts, path, line);
                e.tokens = delta;
                e.model = model.clone();
                e.measurement = "estimated".into();
                batch.events.push(e);
            }
            Some("event_msg") if p["type"] == "item_completed" => {
                let item = &p["item"];
                let Some(item_id) = common::string(&item["id"]) else {
                    continue;
                };
                if item["type"] == "FileChange" && item["status"] == "completed" {
                    if let Some(changes) = item["changes"].as_object() {
                        for target in changes.keys() {
                            let key = format!("file-change:{item_id}:{target}");
                            if !seen.insert(key.clone()) {
                                continue;
                            }
                            let mut e = common::event(key, &id, "edit", ts, path, line);
                            e.tool = Some("apply_patch".into());
                            e.target = Some(target.clone());
                            batch.events.push(e);
                        }
                    }
                } else if item["type"] == "CommandExecution"
                    && (item["status"] == "failed"
                        || item["exit_code"].as_i64().is_some_and(|c| c != 0))
                    && !is_search_no_match(item)
                {
                    let key = format!("command:{item_id}");
                    if !seen.insert(key.clone()) {
                        continue;
                    }
                    let mut e = common::event(key, &id, "failure", ts, path, line);
                    e.tool = Some("exec".into());
                    common::failure(&mut e, &common::text(&item["aggregated_output"]));
                    if e.category.as_deref() == Some("other") {
                        e.category = Some("command-failed".into());
                    }
                    batch.events.push(e);
                }
            }
            Some("response_item") => {
                let kind = p["type"].as_str().unwrap_or("");
                if kind == "message"
                    && p["role"] == "user"
                    && v["metadata"]["client_authored"] != false
                {
                    let content = common::text(&p["content"]);
                    if content.is_empty() {
                        continue;
                    }
                    let key = common::string(&p["id"])
                        .map(|s| format!("prompt:{s}"))
                        .unwrap_or_else(|| {
                            format!("prompt:{ts:?}:{}", common::fingerprint(&content))
                        });
                    if !seen.insert(key.clone()) {
                        continue;
                    }
                    let e = common::event(key, &id, "prompt", ts, path, line);
                    batch.events.extend(common::prompt(e, &content));
                } else if matches!(kind, "function_call" | "custom_tool_call") {
                    if let Some(call) = common::string(&p["call_id"]) {
                        let name = common::string(&p["name"]).unwrap_or_else(|| "unknown".into());
                        let input = p
                            .get("arguments")
                            .or_else(|| p.get("input"))
                            .cloned()
                            .unwrap_or(Value::Null);
                        let input = input
                            .as_str()
                            .and_then(|s| serde_json::from_str(s).ok())
                            .unwrap_or(input);
                        calls.insert(call, (name, input));
                    }
                } else if matches!(kind, "function_call_output" | "custom_tool_call_output") {
                    // Older rollouts have no lifecycle events. Only a structural
                    // error flag or the tool's exit-code marker establishes failure.
                    let output = common::text(&p["output"]);
                    if p["is_error"] == true || output.lines().any(common::nonzero_exit) {
                        let call = p["call_id"].as_str().unwrap_or("");
                        if lifecycle_calls.contains(call) {
                            continue;
                        }
                        let key = format!("result:{call}:{ts:?}");
                        if !seen.insert(key.clone()) {
                            continue;
                        }
                        let mut e = common::event(key, &id, "failure", ts, path, line);
                        e.tool = calls.get(call).map(|(n, _)| n.clone());
                        common::failure(&mut e, &output);
                        batch.events.push(e);
                    }
                }
            }
            Some("compacted") => {
                let key = format!("compaction:{}", p["window_id"]);
                let e = common::event(key, &id, "compaction", ts, path, line);
                batch.events.push(e);
            }
            Some("event_msg" | "world_state" | "inter_agent_communication_metadata") => {}
            _ => batch.unknown_records += 1,
        }
    }
    batch
}

fn is_search_no_match(item: &Value) -> bool {
    if item["exit_code"].as_i64() != Some(1)
        || item["aggregated_output"].as_str().map(str::trim) != Some("")
    {
        return false;
    }
    let Some(args) = item["command"].as_array() else {
        return false;
    };
    let Some(executable) = args.first().and_then(Value::as_str) else {
        return false;
    };
    let name = std::path::Path::new(executable)
        .file_name()
        .and_then(|name| name.to_str());
    if name == Some("rg") {
        return args.iter().all(Value::is_string);
    }
    let [_, flag, script] = args.as_slice() else {
        return false;
    };
    if !matches!(name, Some("sh" | "bash" | "zsh")) || !matches!(flag.as_str(), Some("-c" | "-lc"))
    {
        return false;
    }
    let Some(script) = script
        .as_str()
        .and_then(|s| s.strip_prefix("rg"))
        .filter(|s| s.starts_with(' ') || s.starts_with('\t'))
    else {
        return false;
    };
    // Reject shell composition and expansion rather than hiding another command's failure.
    let mut quote = None;
    for c in script.chars() {
        if quote == Some('\'') {
            if c == '\'' {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' if quote.is_none() => quote = Some(c),
            '"' => quote = if quote.is_some() { None } else { Some(c) },
            '$' | '`' | '\\' => return false,
            '#' | '|' | '&' | ';' | '<' | '>' | '(' | ')' | '\n' | '\r' if quote.is_none() => {
                return false;
            }
            _ => {}
        }
    }
    quote.is_none()
}

fn tokens(v: &Value) -> Tokens {
    Tokens {
        input: v["input_tokens"].as_u64(),
        output: v["output_tokens"].as_u64(),
        cache_read: v["cached_input_tokens"].as_u64(),
        cache_write: v["cache_write_input_tokens"].as_u64(),
        reasoning: v["reasoning_output_tokens"].as_u64(),
        total: v["total_tokens"].as_u64(),
    }
}
fn subtract(a: &Tokens, b: &Tokens) -> Tokens {
    let d = |a: Option<u64>, b: Option<u64>| a.zip(b).map(|(a, b)| a.saturating_sub(b));
    Tokens {
        input: d(a.input, b.input),
        output: d(a.output, b.output),
        cache_read: d(a.cache_read, b.cache_read),
        cache_write: d(a.cache_write, b.cache_write),
        reasoning: d(a.reasoning, b.reasoning),
        total: d(a.total, b.total),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retains_legacy_prefix_and_counts_exec_errors_once() {
        let records = [
            serde_json::json!({"type":"session_meta","payload":{"id":"s","cwd":"/work"}}),
            serde_json::json!({"type":"event_msg","timestamp":"2026-01-01T00:00:01Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12},"last_token_usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12}}}}),
            serde_json::json!({"type":"token_usage_record","timestamp":"2026-01-01T00:00:02Z","payload":{"response_id":"r","usage":{"input_tokens":30,"output_tokens":5,"total_tokens":35}}}),
            serde_json::json!({"type":"event_msg","timestamp":"2026-01-01T00:00:02Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":40,"output_tokens":7,"total_tokens":47}}}}),
            serde_json::json!({"type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"old","arguments":"{}"}}),
            serde_json::json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"old","output":"Process exited with code 1"}}),
            serde_json::json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"modern","output":"Process exited with code 1"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","item":{"type":"CommandExecution","id":"modern","status":"failed","exit_code":1}}}),
        ];
        let text = records
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let batch = parse(&text, "synthetic");
        assert_eq!(
            batch
                .events
                .iter()
                .filter_map(|e| e.tokens.total)
                .sum::<u64>(),
            47
        );
        assert_eq!(
            batch.events.iter().filter(|e| e.kind == "failure").count(),
            2
        );
        assert!(
            batch
                .events
                .iter()
                .any(|e| e.kind == "usage" && e.measurement == "estimated")
        );
    }

    #[test]
    fn harness_injected_user_messages_are_not_prompts() {
        let records = [
            serde_json::json!({"type":"session_meta","payload":{"id":"s","cwd":"/work"}}),
            serde_json::json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"# AGENTS.md instructions for /work\n\n<INSTRUCTIONS>\nnope\n</INSTRUCTIONS>"}]}}),
            serde_json::json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>\n  <cwd>/work</cwd>\n</environment_context>"}]}}),
            serde_json::json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<recommended_plugins>\n</recommended_plugins>"}]}}),
            serde_json::json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<turn_aborted>\n</turn_aborted>"}]}}),
            serde_json::json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"いや、戻して?"}]}}),
        ];
        let text = records
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let batch = parse(&text, "synthetic");
        let prompts: Vec<_> = batch.events.iter().filter(|e| e.kind == "prompt").collect();
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0].category.as_deref(), Some("correct"));
    }

    #[test]
    fn cumulative_repeats_and_resets_are_not_added_twice() {
        let b = parse(
            concat!(
                "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s\"}}\n",
                "{\"type\":\"event_msg\",\"timestamp\":\"2026-01-01T00:00:01Z\",\"payload\":{\"type\":\"token_count\",\"info\":{\"total_token_usage\":{\"input_tokens\":10,\"output_tokens\":5,\"total_tokens\":15}}}}\n",
                "{\"type\":\"event_msg\",\"timestamp\":\"2026-01-01T00:00:02Z\",\"payload\":{\"type\":\"token_count\",\"info\":{\"total_token_usage\":{\"input_tokens\":10,\"output_tokens\":5,\"total_tokens\":15}}}}\n",
                "{\"type\":\"event_msg\",\"timestamp\":\"2026-01-01T00:00:03Z\",\"payload\":{\"type\":\"token_count\",\"info\":{\"total_token_usage\":{\"input_tokens\":3,\"output_tokens\":2,\"total_tokens\":5}}}}\n"
            ),
            "synthetic.jsonl",
        );
        assert_eq!(b.events.len(), 2);
        assert_eq!(
            b.events.iter().filter_map(|e| e.tokens.total).sum::<u64>(),
            20
        );
        assert_eq!(b.warnings.len(), 1);
    }
}
