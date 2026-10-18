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

/// Finalize normalized identity and project-relative edit paths before storage.
pub fn normalize(batch: &mut Batch) {
    for event in &mut batch.events {
        // A multi-file patch produces distinct edit events under one source ID.
        event.id = format!(
            "{}:{}{}",
            event.kind,
            event.id,
            event
                .target
                .as_ref()
                .map(|s| format!(":{}", fingerprint(s)))
                .unwrap_or_default()
        );
        if event.kind == "edit"
            && let Some(target) = &event.target
            && !std::path::Path::new(target).is_absolute()
            && let Some(project) = batch
                .sessions
                .iter()
                .find(|s| s.id == event.session_id)
                .and_then(|s| s.project.as_deref())
        {
            event.target = Some(
                std::path::Path::new(project)
                    .join(target)
                    .display()
                    .to_string(),
            );
        }
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

pub fn prompt(mut e: Event, content: &str) -> Option<Event> {
    // These runtimes also write machine context as user-role text.
    const CONTEXT_PREFIXES: &[&str] = &[
        "# AGENTS.md instructions for",
        "<permissions instructions>",
        "<environment_context>",
        "<recommended_plugins>",
        "<turn_aborted>",
        "<subagent_notification>",
        "<task-notification>",
        "<external_codex_apps_open_page>",
        "[Request interrupted by user",
        "This session is being continued from a previous conversation that ran out of context.",
    ];
    let head = content.trim_start();
    if head.is_empty()
        || CONTEXT_PREFIXES
            .iter()
            .any(|prefix| head.starts_with(prefix))
    {
        return None;
    }
    e.category = Some(crate::core::prompt::classify_prompt(content).label().into());
    e.measurement = "estimated".into();
    Some(e)
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

pub fn fingerprint_reader(mut reader: impl std::io::Read) -> std::io::Result<String> {
    use std::hash::Hasher;
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = match reader.read(&mut buffer) {
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if count == 0 {
            break;
        }
        hash.write(&buffer[..count]);
    }
    // Match str::hash's terminating marker, preserving existing JSONL fingerprints.
    hash.write_u8(0xff);
    Ok(format!("{:016x}", hash.finish()))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_fingerprint_matches_text_and_detects_same_size_rewrites() {
        let text = format!("{}tail", "合成データ\n".repeat(10000));
        assert_eq!(
            fingerprint_reader(text.as_bytes()).unwrap(),
            fingerprint(&text)
        );
        let changed = text.replace("tail", "edit");
        assert_ne!(
            fingerprint_reader(changed.as_bytes()).unwrap(),
            fingerprint(&text)
        );
        assert_eq!(
            fingerprint_reader(std::io::empty()).unwrap(),
            fingerprint("")
        );
    }
}
