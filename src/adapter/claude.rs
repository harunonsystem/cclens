//! Claude adapter for complete response usage, alongside the original skill analysis.
use super::multi as common;
use crate::multi::{Batch, Event, Session, Tokens};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};

pub fn parse(text: &str, path: &str) -> Batch {
    let mut batch = Batch::default();
    let rows = common::lines(text, &mut batch);
    let id = rows
        .iter()
        .find_map(|(_, v)| common::string(&v["sessionId"]))
        .unwrap_or_else(|| path.into());
    if !rows.iter().any(|(_, value)| value["sessionId"].is_string()) {
        batch
            .warnings
            .push("missing Claude session identity; source coverage is incomplete".into());
    }
    batch.sessions.push(Session {
        id: id.clone(),
        project: rows.iter().find_map(|(_, v)| common::string(&v["cwd"])),
        started_ms: rows
            .iter()
            .find_map(|(_, v)| common::timestamp(&v["timestamp"])),
        ..Default::default()
    });
    let mut usage: BTreeMap<String, Event> = BTreeMap::new();
    let mut seen = HashSet::new();
    let mut calls: HashMap<String, (String, Value)> = HashMap::new();
    for (line, v) in rows {
        let ts = common::timestamp(&v["timestamp"]);
        let key = common::string(&v["uuid"]).unwrap_or_else(|| format!("{path}:{line}"));
        let m = &v["message"];
        if v["type"] == "assistant" {
            if m["model"] != "<synthetic>" {
                let response = common::string(&m["id"]).unwrap_or_else(|| key.clone());
                let e = usage.entry(response.clone()).or_insert_with(|| {
                    common::event(format!("response:{response}"), &id, "usage", ts, path, line)
                });
                e.model = common::string(&m["model"]);
                let u = &m["usage"];
                let i = u["input_tokens"].as_u64();
                let r = u["cache_read_input_tokens"].as_u64();
                let w = u["cache_creation_input_tokens"].as_u64();
                let input = i
                    .zip(r)
                    .zip(w)
                    .and_then(|((i, r), w)| i.checked_add(r)?.checked_add(w));
                let output = u["output_tokens"].as_u64();
                let max = |a: Option<u64>, b: Option<u64>| match (a, b) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    (a, b) => a.or(b),
                };
                e.tokens = Tokens {
                    input: max(e.tokens.input, input),
                    output: max(e.tokens.output, output),
                    cache_read: max(e.tokens.cache_read, r),
                    cache_write: max(e.tokens.cache_write, w),
                    reasoning: None,
                    total: None,
                };
                e.tokens.total = e
                    .tokens
                    .input
                    .zip(e.tokens.output)
                    .and_then(|(i, o)| i.checked_add(o));
                if e.tokens == Tokens::default() {
                    e.measurement = "unavailable".into();
                } else {
                    e.measurement = "measured".into();
                }
            }
            if let Some(blocks) = m["content"].as_array() {
                for b in blocks {
                    if b["type"] != "tool_use" {
                        continue;
                    }
                    let Some(call) = common::string(&b["id"]) else {
                        continue;
                    };
                    let tool = common::string(&b["name"]).unwrap_or_else(|| "unknown".into());
                    calls.insert(call.clone(), (tool.clone(), b["input"].clone()));
                    if tool == "Skill" && seen.insert(format!("skill:{call}")) {
                        let mut e =
                            common::event(format!("skill:{call}"), &id, "skill", ts, path, line);
                        e.tool = Some(tool);
                        e.target = common::string(&b["input"]["skill"]);
                        e.category = Some("invocation".into());
                        batch.events.push(e);
                    }
                }
            }
        } else if v["type"] == "user" {
            let blocks = m["content"].as_array();
            let mut has_result = false;
            if let Some(blocks) = blocks {
                for b in blocks {
                    if b["type"] != "tool_result" {
                        continue;
                    }
                    has_result = true;
                    let call = b["tool_use_id"].as_str().unwrap_or("");
                    let result_key = format!("result:{call}:{key}");
                    if !seen.insert(result_key.clone()) {
                        continue;
                    }
                    let output = common::text(&b["content"]);
                    let failed = b["is_error"] == true || output.contains("<tool_use_error>");
                    if failed {
                        let mut e = common::event(result_key, &id, "failure", ts, path, line);
                        e.tool = calls.get(call).map(|(n, _)| n.clone());
                        common::failure(&mut e, &output);
                        if v["toolDenialKind"] == "permission-rule" {
                            e.category = Some("blocked-by-hook".into());
                        }
                        if v["toolDenialKind"] == "user-rejected" {
                            e.category = Some("cancelled".into());
                        }
                        batch.events.push(e);
                    } else if let Some((tool, input)) = calls.get(call) {
                        for target in common::edit_paths(tool, input) {
                            let mut e = common::event(
                                format!("edit:{call}:{target}"),
                                &id,
                                "edit",
                                ts,
                                path,
                                line,
                            );
                            e.tool = Some(tool.clone());
                            e.target = Some(target);
                            batch.events.push(e);
                        }
                    }
                }
            }
            if !has_result && v["isMeta"] != true {
                let content = common::text(&m["content"]);
                if !content.is_empty()
                    && !content.trim_start().starts_with("<command-")
                    && seen.insert(format!("prompt:{key}"))
                {
                    let mut e =
                        common::event(format!("prompt:{key}"), &id, "prompt", ts, path, line);
                    common::prompt(&mut e, &content);
                    batch.events.push(e);
                }
            }
        } else if v["type"] == "system" && v["subtype"] == "compact_boundary" {
            batch.events.push(common::event(
                format!("compaction:{key}"),
                &id,
                "compaction",
                ts,
                path,
                line,
            ));
        } else if !matches!(
            v["type"].as_str(),
            Some(
                "system"
                    | "permission-mode"
                    | "attachment"
                    | "file-history-snapshot"
                    | "last-prompt"
                    | "mode"
                    | "atis-latch"
                    | "ai-title"
                    | "queue-operation"
                    | "file-history-delta"
                    | "cost-state"
                    | "relocated"
                    | "worktree-state"
                    | "pr-link"
                    | "agent-name"
                    | "frame-link"
                    | "artifact-comment-monitor"
                    | "artifact-autoreact-ledger"
                    | "continued-in"
                    | "fork-context-ref"
            )
        ) {
            batch.unknown_records += 1;
        }
    }
    batch.events.extend(usage.into_values());
    batch
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn streamed_response_uses_maxima_instead_of_summing_blocks() {
        let b = parse(
            concat!(
                "{\"type\":\"assistant\",\"message\":{\"id\":\"r\",\"usage\":{\"input_tokens\":2,\"cache_read_input_tokens\":3,\"cache_creation_input_tokens\":1,\"output_tokens\":4}}}\n",
                "{\"type\":\"assistant\",\"message\":{\"id\":\"r\",\"usage\":{\"input_tokens\":2,\"cache_read_input_tokens\":3,\"cache_creation_input_tokens\":1,\"output_tokens\":7}}}\n"
            ),
            "synthetic.jsonl",
        );
        assert_eq!(b.events.len(), 1);
        assert_eq!(b.events[0].tokens.total, Some(13));
    }
}
