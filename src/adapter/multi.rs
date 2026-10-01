//! Shared input helpers for the four adapters.
use crate::multi::{Batch, Event, Evidence};
use serde_json::Value;

pub fn timestamp(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| {
        value.as_str().and_then(|s| {
            chrono::DateTime::parse_from_rfc3339(s)
                .ok()
                .map(|d| d.timestamp_millis())
        })
    })
}

pub fn string(value: &Value) -> Option<String> {
    value.as_str().filter(|s| !s.is_empty()).map(str::to_owned)
}

pub fn lines(text: &str, batch: &mut Batch) -> Vec<(usize, Value)> {
    text.lines()
        .enumerate()
        .filter_map(|(i, line)| {
            if line.trim().is_empty() {
                return None;
            }
            match serde_json::from_str(line) {
                Ok(v) => Some((i + 1, v)),
                Err(_) => {
                    batch
                        .warnings
                        .push(format!("invalid JSON at line {}", i + 1));
                    None
                }
            }
        })
        .collect()
}

pub fn event(
    id: String,
    session: &str,
    kind: &str,
    ts: Option<i64>,
    path: &str,
    line: usize,
) -> Event {
    Event {
        id,
        session_id: session.to_owned(),
        kind: kind.to_owned(),
        timestamp_ms: ts,
        measurement: "measured".into(),
        evidence: Evidence {
            path: path.into(),
            line: Some(line),
            ..Default::default()
        },
        ..Default::default()
    }
}

pub fn text(value: &Value) -> String {
    if let Some(s) = value.as_str() {
        return s.into();
    }
    value
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

pub fn prompt(e: &mut Event, content: &str) {
    e.category = Some(crate::core::prompt::classify_prompt(content).label().into());
    e.measurement = "estimated".into();
}

pub fn failure(e: &mut Event, content: &str) {
    e.category = Some(
        crate::core::friction::classify_error(content)
            .label()
            .into(),
    );
    // The failure itself is measured; its textual category is a heuristic.
}

pub fn edit_paths(tool: &str, input: &Value) -> Vec<String> {
    let name = tool
        .rsplit(['.', '_'])
        .next()
        .unwrap_or(tool)
        .to_lowercase();
    if matches!(name.as_str(), "edit" | "write" | "multiedit") {
        return string(&input["file_path"])
            .or_else(|| string(&input["filePath"]))
            .or_else(|| string(&input["path"]))
            .into_iter()
            .collect();
    }
    if tool.ends_with("apply_patch") || tool.ends_with("applyPatch") {
        let patch = input
            .as_str()
            .or_else(|| input["patchText"].as_str())
            .or_else(|| input["patch"].as_str())
            .or_else(|| input["input"].as_str())
            .unwrap_or("");
        return patch
            .lines()
            .filter_map(|line| {
                [
                    "*** Update File: ",
                    "*** Add File: ",
                    "*** Delete File: ",
                    "*** Move to: ",
                ]
                .iter()
                .find_map(|prefix| line.strip_prefix(prefix).map(str::to_owned))
            })
            .collect();
    }
    Vec::new()
}

pub fn fingerprint(text: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    format!("{:016x}", h.finish())
}

pub fn nonzero_exit(line: &str) -> bool {
    [
        "Process exited with code ",
        "Process exited with exit code ",
        "exit code: ",
        "Exit code: ",
    ]
    .iter()
    .any(|prefix| {
        line.trim()
            .strip_prefix(prefix)
            .and_then(|s| s.split_whitespace().next())
            .and_then(|s| s.parse::<i64>().ok())
            .is_some_and(|code| code != 0)
    })
}
