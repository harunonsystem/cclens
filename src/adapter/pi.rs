//! Adapter for pi's session JSONL files.
use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::{
    adapter::multi as common,
    multi::{Batch, Session, Tokens},
};

#[derive(Clone)]
struct Call {
    name: String,
    arguments: Value,
}

/// Parse a pi session file into the shared event model.
pub fn parse(text: &str, path: &str) -> Batch {
    let mut batch = Batch::default();
    let records = common::lines(text, &mut batch);
    let header = records
        .iter()
        .find(|(_, v)| v["type"] == "session")
        .map(|(_, v)| v.clone());
    if header.is_none()
        && std::path::Path::new(path).ends_with("observation-pack/ledger.jsonl")
        && !records.is_empty()
        && records.iter().all(|(_, record)| {
            record.get("type").is_none()
                && matches!(
                    record["event"].as_str(),
                    Some("full" | "placeholder" | "recall")
                )
                && record["id"].is_string()
                && record["timestamp"].is_string()
        })
    {
        batch.warnings.push(format!(
            "{} auxiliary observation records excluded from session usage",
            records.len()
        ));
        return batch;
    }
    let session_id = header
        .as_ref()
        .and_then(|v| common::string(&v["id"]))
        .unwrap_or_else(|| path.to_owned());
    if header.is_none() {
        batch.warnings.push(
            "missing pi session header; this file may be an unsupported auxiliary log".into(),
        );
    }
    let mut session = Session {
        id: session_id.clone(),
        project: header.as_ref().and_then(|v| common::string(&v["cwd"])),
        started_ms: header
            .as_ref()
            .and_then(|v| common::timestamp(&v["timestamp"])),
        ..Default::default()
    };
    let mut seen = HashSet::new();
    let mut calls: HashMap<String, Call> = HashMap::new();
    let mut tool_results: HashSet<String> = HashSet::new();
    let mut current_model = header.as_ref().and_then(|v| common::string(&v["modelId"]));

    for (line, record) in records {
        match record["type"].as_str() {
            Some("session") => {}
            Some(
                "custom"
                | "custom_message"
                | "thinking_level_change"
                | "session_info"
                | "context_edit"
                | "branch_summary"
                | "label"
                | "title"
                | "title_change"
                | "credential_pin"
                | "session_init"
                | "mode_change"
                | "service_tier_change"
                | "ttsr_injection",
            ) => {}
            Some("compaction") => {
                let Some(id) = common::string(&record["id"]) else {
                    batch.unknown_records += 1;
                    continue;
                };
                if seen.insert(id.clone()) {
                    let mut event = common::event(
                        id,
                        &session_id,
                        "compaction",
                        common::timestamp(&record["timestamp"]),
                        path,
                        line,
                    );
                    event.measurement = "unavailable".into();
                    batch.events.push(event);
                }
            }
            Some("model_change") => {
                current_model = common::string(&record["modelId"])
                    .or_else(|| common::string(&record["model"]))
                    .or_else(|| common::string(&record["providerModel"]));
                if current_model.is_none() {
                    batch.unknown_records += 1;
                }
            }
            Some("message") => {
                let Some(entry_id) = common::string(&record["id"]) else {
                    batch.unknown_records += 1;
                    continue;
                };
                // Entry ids survive copied fork prefixes; only the first copy contributes.
                if !seen.insert(entry_id.clone()) {
                    continue;
                }
                let message = &record["message"];
                let role = message["role"].as_str().unwrap_or("");
                let ts = common::timestamp(&record["timestamp"]);
                if session.started_ms.is_none() {
                    session.started_ms = ts;
                }
                let content = &message["content"];
                let content_text = common::text(content);
                let model = common::string(&message["model"]).or_else(|| current_model.clone());
                if role == "user" {
                    let event = common::event(entry_id, &session_id, "prompt", ts, path, line);
                    batch.events.extend(common::prompt(event, &content_text));
                    continue;
                }
                if role == "assistant" {
                    {
                        let usage = &message["usage"];
                        let input = usage["input"].as_u64();
                        let cache_read = usage["cacheRead"].as_u64();
                        let cache_write = usage["cacheWrite"].as_u64();
                        let inclusive_input = match (input, cache_read, cache_write) {
                            (Some(i), Some(r), Some(w)) => {
                                i.checked_add(r).and_then(|n| n.checked_add(w))
                            }
                            _ => None,
                        };
                        let cost = usage["cost"]["total"].as_f64();
                        let mut event =
                            common::event(entry_id.clone(), &session_id, "usage", ts, path, line);
                        if usage.is_null() {
                            event.measurement = "unavailable".into();
                        }
                        event.model = model;
                        event.tokens = Tokens {
                            input: inclusive_input,
                            output: usage["output"].as_u64(),
                            cache_read,
                            cache_write,
                            reasoning: usage["reasoning"]
                                .as_u64()
                                .or_else(|| usage["reasoningTokens"].as_u64()),
                            total: usage["totalTokens"].as_u64(),
                        };
                        event.api_cost_usd = cost;
                        batch.events.push(event);
                    }
                    if let Some(parts) = content.as_array() {
                        for part in parts {
                            if part["type"] != "toolCall" {
                                continue;
                            }
                            let Some(id) = common::string(&part["id"]) else {
                                continue;
                            };
                            let name = common::string(&part["name"]).unwrap_or_default();
                            let arguments = part.get("arguments").cloned().unwrap_or(Value::Null);
                            calls.insert(id, Call { name, arguments });
                        }
                    }
                } else if role == "toolResult" {
                    let Some(call_id) = common::string(&message["toolCallId"]) else {
                        batch.unknown_records += 1;
                        continue;
                    };
                    if !tool_results.insert(call_id.clone()) {
                        continue;
                    }
                    let call = calls.get(&call_id);
                    let result_text = if content.is_null() {
                        common::text(&message["details"])
                    } else {
                        common::text(content)
                    };
                    if message["isError"].as_bool() == Some(true) {
                        let mut event = common::event(
                            format!("{entry_id}:failure"),
                            &session_id,
                            "failure",
                            ts,
                            path,
                            line,
                        );
                        event.tool = call.map(|call| call.name.clone());
                        common::failure(&mut event, &result_text);
                        batch.events.push(event);
                    }
                    let Some(call) = call else {
                        batch.unknown_records += 1;
                        continue;
                    };
                    if message["isError"].as_bool() == Some(false) {
                        for target in common::edit_paths(&call.name, &call.arguments) {
                            let mut event = common::event(
                                format!("{entry_id}:edit:{target}"),
                                &session_id,
                                "edit",
                                ts,
                                path,
                                line,
                            );
                            event.tool = Some(call.name.clone());
                            event.target = Some(target);
                            batch.events.push(event);
                        }
                    }
                    if message["isError"].as_bool() == Some(false)
                        && call
                            .name
                            .rsplit(['.', '_'])
                            .next()
                            .unwrap_or(&call.name)
                            .eq_ignore_ascii_case("read")
                        && let Some(read_path) = common::string(&call.arguments["path"])
                            .or_else(|| common::string(&call.arguments["file_path"]))
                            .filter(|p| p.ends_with("SKILL.md"))
                    {
                        let skill = read_path
                            .trim_end_matches("/SKILL.md")
                            .rsplit('/')
                            .next()
                            .unwrap_or("");
                        if !skill.is_empty() {
                            let mut event = common::event(
                                format!("{entry_id}:skill:{skill}"),
                                &session_id,
                                "skill",
                                ts,
                                path,
                                line,
                            );
                            event.category = Some("read".into());
                            event.target = Some(skill.to_owned());
                            event.measurement = "estimated".into();
                            batch.events.push(event);
                        }
                    }
                } else if !matches!(role, "system" | "developer" | "bashExecution") {
                    batch.unknown_records += 1;
                }
            }
            _ => batch.unknown_records += 1,
        }
    }
    if let Some(parent) = header.as_ref().and_then(|v| common::string(&v["parentId"])) {
        session.parent_id = Some(parent);
    }
    // Short IDs are not agent-wide identities. Copied fork prefixes retain timestamps.
    for event in &mut batch.events {
        event.id = format!(
            "{}:{}",
            event.id,
            event
                .timestamp_ms
                .map(|t| t.to_string())
                .unwrap_or_else(|| event.session_id.clone())
        );
    }
    batch.sessions.push(session);
    batch
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_lines(lines: &[Value]) -> Batch {
        parse(
            &lines
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
            "session.jsonl",
        )
    }
    fn rec(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn auxiliary_ledger_is_identified_without_claiming_session_usage() {
        let batch = parse(
            r#"{"event":"full","id":"obs-a","timestamp":"2026-01-01T00:00:00Z","originalTokens":9}
{"event":"placeholder","id":"obs-b","timestamp":"2026-01-01T00:00:01Z","originalTokens":10}
{"event":"recall","id":"obs-a","timestamp":"2026-01-01T00:00:02Z","bytes":12}"#,
            "/tmp/example/observation-pack/ledger.jsonl",
        );
        assert!(batch.sessions.is_empty());
        assert!(batch.events.is_empty());
        assert_eq!(batch.unknown_records, 0);
        assert!(
            batch
                .warnings
                .iter()
                .any(|w| w.contains("3 auxiliary observation records"))
        );
        let unknown = parse(
            r#"{"event":"new-unknown","id":"obs-c"}"#,
            "/tmp/example/observation-pack/ledger.jsonl",
        );
        assert_eq!(unknown.unknown_records, 1);
    }

    #[test]
    fn deduplicates_copied_fork_entries_and_ignores_compaction_usage() {
        let b = parse_lines(&[
            rec(
                r#"{"type":"session","id":"s","cwd":"/work","timestamp":"2026-01-01T00:00:00Z","version":3}"#,
            ),
            rec(
                r#"{"type":"message","id":"same","parentId":null,"timestamp":"2026-01-01T00:00:01Z","message":{"role":"assistant","model":"m","content":[],"usage":{"input":2,"output":3,"cacheRead":0,"cacheWrite":0,"totalTokens":5}}}"#,
            ),
            rec(
                r#"{"type":"compaction","id":"compact","timestamp":"2026-01-01T00:00:02Z","usage":{"input":99,"output":99}}"#,
            ),
            rec(
                r#"{"type":"message","id":"same","timestamp":"2026-01-01T00:00:03Z","message":{"role":"assistant","model":"m","content":[],"usage":{"input":2,"output":3,"cacheRead":0,"cacheWrite":0,"totalTokens":5}}}"#,
            ),
        ]);
        assert_eq!(b.events.iter().filter(|e| e.kind == "usage").count(), 1);
        assert_eq!(
            b.events.iter().filter(|e| e.kind == "compaction").count(),
            1
        );
        assert_eq!(b.events[0].tokens.input, Some(2));
        assert_eq!(b.events[0].tokens.output, Some(3));
    }

    #[test]
    fn model_change_is_fallback_and_message_model_wins() {
        let b = parse_lines(&[
            rec(r#"{"type":"session","id":"s","cwd":"/work","modelId":"header","version":3}"#),
            rec(
                r#"{"type":"message","id":"header-model","message":{"role":"assistant","content":[],"usage":{"input":1,"output":2,"cacheRead":0,"cacheWrite":0}}}"#,
            ),
            rec(r#"{"type":"model_change","modelId":"fallback"}"#),
            rec(
                r#"{"type":"message","id":"a","message":{"role":"assistant","content":[],"usage":{"input":1,"output":2,"cacheRead":0,"cacheWrite":0}}}"#,
            ),
            rec(r#"{"type":"model_change","model":"new"}"#),
            rec(
                r#"{"type":"message","id":"b","message":{"role":"assistant","model":"native","content":[],"usage":{"input":1,"output":2,"cacheRead":0,"cacheWrite":0}}}"#,
            ),
        ]);
        assert_eq!(b.events[0].model.as_deref(), Some("header"));
        assert_eq!(b.events[1].model.as_deref(), Some("fallback"));
        assert_eq!(b.events[2].model.as_deref(), Some("native"));
    }

    #[test]
    fn missing_usage_is_an_unavailable_usage_event() {
        let b = parse_lines(&[
            rec(r#"{"type":"session","id":"s","cwd":"/work","version":3}"#),
            rec(r#"{"type":"message","id":"a","message":{"role":"assistant","content":[]}}"#),
        ]);
        assert_eq!(b.events.len(), 1);
        assert_eq!(b.events[0].kind, "usage");
        assert_eq!(b.events[0].measurement, "unavailable");
        assert_eq!(b.events[0].tokens, Tokens::default());
    }

    #[test]
    fn retains_known_error_when_tool_call_is_missing() {
        let batch = parse_lines(&[
            rec(r#"{"type":"session","id":"s","version":3}"#),
            rec(
                r#"{"type":"message","id":"r","message":{"role":"toolResult","toolCallId":"unknown","content":"private failure body","isError":true}}"#,
            ),
        ]);
        assert_eq!(batch.events.len(), 1);
        assert_eq!(batch.events[0].kind, "failure");
        assert!(batch.events[0].tool.is_none());
        assert_eq!(batch.unknown_records, 1);
        assert!(
            !serde_json::to_string(&batch)
                .unwrap()
                .contains("private failure body")
        );
    }

    #[test]
    fn links_tool_failures_edits_prompts_and_skill_reads_without_storing_bodies() {
        let b = parse_lines(&[
            rec(r#"{"type":"session","id":"s","cwd":"/work","version":3}"#),
            rec(
                r#"{"type":"message","id":"u","timestamp":"2026-01-01T00:00:00Z","message":{"role":"user","content":"Please inspect the project"}}"#,
            ),
            rec(
                r#"{"type":"message","id":"a","timestamp":"2026-01-01T00:00:01Z","message":{"role":"assistant","content":[{"type":"toolCall","id":"c","name":"edit","arguments":{"file_path":"src/main.rs","oldText":"secret","newText":"change"}},{"type":"toolCall","id":"ok","name":"write","arguments":{"file_path":"src/ok.rs","content":"private body"}},{"type":"toolCall","id":"sread","name":"read","arguments":{"path":"/home/user/.pi/skills/review/SKILL.md"}}]}}"#,
            ),
            rec(
                r#"{"type":"message","id":"r","timestamp":"2026-01-01T00:00:02Z","message":{"role":"toolResult","toolCallId":"c","content":"write failed","isError":true}}"#,
            ),
            rec(
                r#"{"type":"message","id":"ok-result","timestamp":"2026-01-01T00:00:02Z","message":{"role":"toolResult","toolCallId":"ok","content":"written","isError":false}}"#,
            ),
            rec(
                r#"{"type":"message","id":"sr","timestamp":"2026-01-01T00:00:03Z","message":{"role":"toolResult","toolCallId":"sread","content":"skill contents","isError":false}}"#,
            ),
        ]);
        assert!(b.events.iter().any(|e| {
            e.kind == "prompt" && e.evidence.line == Some(2) && e.evidence.path == "session.jsonl"
        }));
        assert!(b.events.iter().any(|e| e.kind == "failure"));
        assert!(
            b.events
                .iter()
                .any(|e| e.kind == "edit" && e.target.as_deref() == Some("src/ok.rs"))
        );
        assert!(
            !b.events
                .iter()
                .any(|e| e.kind == "edit" && e.target.as_deref() == Some("src/main.rs"))
        );
        assert!(
            b.events
                .iter()
                .any(|e| e.kind == "skill" && e.target.as_deref() == Some("review"))
        );
        assert!(
            b.events
                .iter()
                .all(|e| !e.id.contains("secret") && !e.id.contains("skill contents"))
        );
    }
}
