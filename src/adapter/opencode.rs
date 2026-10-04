//! Read-only adapter for OpenCode's session database.
use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use serde_json::Value;

use super::multi;
use crate::multi::{Batch, Event, Session, Tokens};

pub fn read_if_changed(
    path: &Path,
    unchanged: impl FnOnce(&str) -> Result<bool>,
) -> Result<Option<(String, Batch)>> {
    let mut conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("open OpenCode database {}", path.display()))?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Deferred)
        .context("start OpenCode read snapshot")?;
    let fingerprint = snapshot_fingerprint(&tx)?;
    if unchanged(&fingerprint)? {
        tx.commit().context("finish unchanged OpenCode snapshot")?;
        return Ok(None);
    }
    let mut batch = Batch::default();

    {
        let mut stmt = tx.prepare(
            "SELECT id, directory, parent_id, time_created FROM session ORDER BY time_created, id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<i64>>(3)?,
            ))
        })?;
        for row in rows {
            let (id, project, parent_id, started_ms) = row?;
            batch.sessions.push(Session {
                id,
                project,
                parent_id,
                started_ms,
            });
        }
    }
    let parents: std::collections::HashMap<_, _> = batch
        .sessions
        .iter()
        .map(|s| (s.id.clone(), s.parent_id.clone()))
        .collect();
    let mut messages = Vec::new();
    {
        let mut stmt = tx.prepare("SELECT id, session_id, data FROM message ORDER BY rowid")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (id, session_id, raw) = row?;
            let data: Value = match serde_json::from_str(&raw) {
                Ok(v) => v,
                Err(_) => {
                    batch
                        .warnings
                        .push(format!("invalid message JSON at message row {id}"));
                    continue;
                }
            };
            messages.push((id, session_id, data));
        }
    }

    let starts: std::collections::HashMap<_, _> = batch
        .sessions
        .iter()
        .map(|s| (s.id.as_str(), s.started_ms))
        .collect();
    let mut signatures: std::collections::HashMap<String, std::collections::HashSet<String>> =
        std::collections::HashMap::new();
    let mut origins: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for (_, session_id, data) in &messages {
        let signature = message_signature(data);
        signatures
            .entry(session_id.clone())
            .or_default()
            .insert(signature.clone());
        origins
            .entry(signature)
            .or_default()
            .push(session_id.clone());
    }
    let mut copied_messages = std::collections::HashSet::new();
    let mut message_info = std::collections::HashMap::new();
    for (id, session_id, data) in messages {
        let role = data["role"].as_str().unwrap_or("");
        let signature = message_signature(&data);
        let mut ancestor = parents.get(&session_id).cloned().flatten();
        let mut walked = std::collections::HashSet::new();
        // Forks preserve message timestamps, including versions without parent_id.
        // ponytail: infer copies from exact metadata predating the fork; no fuzzy matching.
        let created = multi::timestamp(&data["time"]["created"]);
        let start = starts.get(session_id.as_str()).copied().flatten();
        let mut copied = created
            .zip(start)
            .is_some_and(|(created, start)| created < start)
            && origins.get(&signature).is_some_and(|sessions| {
                sessions.iter().any(|other| {
                    other != &session_id
                        && starts
                            .get(other.as_str())
                            .copied()
                            .flatten()
                            .zip(start)
                            .is_some_and(|(original, fork)| original < fork)
                })
            });
        while let Some(parent) = ancestor {
            if !walked.insert(parent.clone()) {
                break;
            }
            if signatures
                .get(&parent)
                .is_some_and(|seen| seen.contains(&signature))
            {
                copied = true;
                break;
            }
            ancestor = parents.get(&parent).cloned().flatten();
        }
        if copied {
            copied_messages.insert(id.clone());
        }
        message_info.insert(
            id.clone(),
            (role.to_owned(), multi::timestamp(&data["time"]["created"])),
        );
        if copied || role != "assistant" {
            continue;
        }
        let tokens = parse_tokens(&data["tokens"]);
        let mut e = base_event(&id, &session_id, "usage", &data, path);
        e.model = match (data["providerID"].as_str(), data["modelID"].as_str()) {
            (Some(provider), Some(model)) => Some(format!("{provider}/{model}")),
            (None, Some(model)) => Some(model.into()),
            _ => None,
        };
        e.tokens = tokens.clone();
        e.api_cost_usd = data["cost"].as_f64();
        if tokens == Tokens::default() && data["cost"].as_f64().is_none() {
            e.measurement = "unavailable".into();
            batch
                .warnings
                .push(format!("missing usage counters at message row {id}"));
        } else if ["input", "output", "reasoning", "total"]
            .iter()
            .any(|key| data["tokens"][*key].as_u64().is_none())
            || data["tokens"]["cache"]["read"].as_u64().is_none()
            || data["tokens"]["cache"]["write"].as_u64().is_none()
        {
            batch.warnings.push(format!(
                "missing or invalid token counters at message row {id}"
            ));
        }
        batch.events.push(e);
    }

    let mut prompt_parts: std::collections::BTreeMap<
        String,
        (String, String, String, Option<i64>),
    > = std::collections::BTreeMap::new();
    {
        let mut stmt =
            tx.prepare("SELECT id, message_id, session_id, data FROM part ORDER BY rowid")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        for row in rows {
            let (id, message_id, session_id, raw) = row?;
            let data: Value = match serde_json::from_str(&raw) {
                Ok(v) => v,
                Err(_) => {
                    batch
                        .warnings
                        .push(format!("invalid part JSON at part row {id}"));
                    continue;
                }
            };
            if copied_messages.contains(&message_id) {
                continue;
            }
            match data["type"].as_str().unwrap_or("") {
                "text" => {
                    if data["synthetic"].as_bool() == Some(true)
                        || data["ignored"].as_bool() == Some(true)
                    {
                        continue;
                    }
                    let Some(text) = data["text"].as_str() else {
                        batch
                            .warnings
                            .push(format!("missing text at part row {id}"));
                        continue;
                    };
                    if message_info
                        .get(&message_id)
                        .is_some_and(|(role, _)| role == "user")
                    {
                        let message_time = message_info.get(&message_id).and_then(|(_, ts)| *ts);
                        let entry = prompt_parts.entry(message_id.clone()).or_insert_with(|| {
                            (session_id.clone(), id.clone(), String::new(), message_time)
                        });
                        if !entry.2.is_empty() {
                            entry.2.push('\n');
                        }
                        entry.2.push_str(text);
                    }
                }
                "tool" => {
                    let tool = data["tool"].as_str().unwrap_or("");
                    let input = &data["state"]["input"];
                    let state = &data["state"];
                    let err = state["error"].as_str().or_else(|| data["error"].as_str());
                    if state["status"].as_str() == Some("error") || err.is_some() {
                        let mut e = part_event(&id, &session_id, "failure", path);
                        e.tool = nonempty(tool);
                        e.timestamp_ms = part_time(state, &message_info, &message_id);
                        multi::failure(&mut e, err.unwrap_or("tool failed"));
                        batch.events.push(e);
                    }
                    if tool == "skill"
                        && let Some(name) = input["name"].as_str().filter(|s| !s.is_empty())
                    {
                        let mut e = part_event(&id, &session_id, "skill", path);
                        e.tool = Some("skill".into());
                        e.target = Some(name.into());
                        e.timestamp_ms = part_time(state, &message_info, &message_id);
                        batch.events.push(e);
                    }
                    let paths = multi::edit_paths(tool, input);
                    if state["status"].as_str() == Some("completed") {
                        for (index, target) in paths.into_iter().enumerate() {
                            // A subsequent patch part carries the same source edit; its
                            // own event is authoritative for patch tools.
                            if tool.ends_with("apply_patch") || tool.ends_with("applyPatch") {
                                continue;
                            }
                            let mut e = part_event(&id, &session_id, "edit", path);
                            if index > 0 {
                                e.id.push_str(&format!(":{index}"));
                            }
                            e.tool = nonempty(tool);
                            e.target = Some(target);
                            e.timestamp_ms = part_time(state, &message_info, &message_id);
                            batch.events.push(e);
                        }
                    }
                }
                "patch" => {
                    let files = data["files"].as_array();
                    if let Some(files) = files {
                        for file in files {
                            if let Some(target) = file.as_str().or_else(|| file["file"].as_str()) {
                                let mut e = part_event(&id, &session_id, "edit", path);
                                e.id = format!("opencode:part:{id}:edit:{target}");
                                e.tool = Some("apply_patch".into());
                                e.target = Some(target.into());
                                e.timestamp_ms =
                                    message_info.get(&message_id).and_then(|(_, ts)| *ts);
                                batch.events.push(e);
                            }
                        }
                    }
                }
                "step-start" | "step-finish" | "reasoning" | "snapshot" | "file" | "source"
                | "agent" | "retry" | "compaction" | "subtask" => {}
                _ => {
                    batch.unknown_records += 1;
                }
            }
        }
    }
    for (_, (session_id, part_id, content, timestamp)) in prompt_parts {
        let mut e = part_event(&part_id, &session_id, "prompt", path);
        e.timestamp_ms = timestamp;
        multi::prompt(&mut e, &content);
        batch.events.push(e);
    }
    tx.commit().context("finish OpenCode read snapshot")?;
    Ok(Some((fingerprint, batch)))
}

fn snapshot_fingerprint(conn: &Connection) -> Result<String> {
    use rusqlite::types::ValueRef;
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    for sql in [
        "SELECT id, directory, parent_id, time_created FROM session ORDER BY time_created, id",
        "SELECT id, session_id, data FROM message ORDER BY rowid",
        "SELECT id, message_id, session_id, data FROM part ORDER BY rowid",
    ] {
        sql.hash(&mut hash);
        let mut stmt = conn.prepare(sql)?;
        let columns = stmt.column_count();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            for i in 0..columns {
                let value = row.get_ref(i)?;
                std::mem::discriminant(&value).hash(&mut hash);
                match value {
                    ValueRef::Null => {}
                    ValueRef::Integer(v) => v.hash(&mut hash),
                    ValueRef::Real(v) => v.to_bits().hash(&mut hash),
                    ValueRef::Text(v) | ValueRef::Blob(v) => v.hash(&mut hash),
                }
            }
        }
    }
    Ok(format!("{:016x}", hash.finish()))
}

fn parse_tokens(v: &Value) -> Tokens {
    let input = v["input"].as_u64();
    let read = v["cache"]["read"].as_u64();
    let write = v["cache"]["write"].as_u64();
    let inclusive_input = input
        .zip(read)
        .zip(write)
        .and_then(|((i, r), w)| i.checked_add(r).and_then(|n| n.checked_add(w)));
    let reasoning = v["reasoning"].as_u64();
    let output = v["output"]
        .as_u64()
        .zip(reasoning)
        .and_then(|(output, reasoning)| output.checked_add(reasoning));
    let derived_total = inclusive_input
        .zip(output)
        .and_then(|(i, o)| i.checked_add(o));
    Tokens {
        input: inclusive_input,
        output,
        cache_read: read,
        cache_write: write,
        reasoning,
        total: v["total"].as_u64().filter(|n| *n > 0).or(derived_total),
    }
}

fn message_signature(value: &Value) -> String {
    let mut value = value.clone();
    if let Some(object) = value.as_object_mut() {
        object.remove("parentID");
    }
    serde_json::to_string(&value).unwrap_or_default()
}

fn part_time(
    state: &Value,
    messages: &std::collections::HashMap<String, (String, Option<i64>)>,
    message_id: &str,
) -> Option<i64> {
    multi::timestamp(&state["time"]["end"])
        .or_else(|| multi::timestamp(&state["time"]["start"]))
        .or_else(|| multi::timestamp(&state["metadata"]["time"]))
        .or_else(|| {
            messages
                .get(message_id)
                .and_then(|(_, timestamp)| *timestamp)
        })
}

fn base_event(id: &str, session: &str, kind: &str, data: &Value, path: &Path) -> Event {
    Event {
        id: format!("opencode:message:{id}"),
        session_id: session.into(),
        timestamp_ms: multi::timestamp(&data["time"]["created"]),
        kind: kind.into(),
        measurement: "measured".into(),
        evidence: crate::multi::Evidence {
            path: path.display().to_string(),
            table: Some("message".into()),
            row_id: Some(id.into()),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn part_event(id: &str, session: &str, kind: &str, path: &Path) -> Event {
    Event {
        id: format!("opencode:part:{id}:{kind}"),
        session_id: session.into(),
        kind: kind.into(),
        measurement: "measured".into(),
        evidence: crate::multi::Evidence {
            path: path.display().to_string(),
            table: Some("part".into()),
            row_id: Some(id.into()),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn nonempty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    #[test]
    fn unchanged_database_is_skipped_before_json_extraction() {
        let dir = std::env::temp_dir().join(format!("cclens-opencode-skip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("input.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
            CREATE TABLE session(id TEXT, directory TEXT, parent_id TEXT, time_created INTEGER); CREATE TABLE message(id TEXT, session_id TEXT, data TEXT); CREATE TABLE part(id TEXT, message_id TEXT, session_id TEXT, data TEXT);
            INSERT INTO message VALUES('synthetic', 'session', 'invalid JSON');").unwrap();
        assert!(read_if_changed(&path, |_| Ok(true)).unwrap().is_none());
        let (fingerprint, batch) = read_if_changed(&path, |_| Ok(false)).unwrap().unwrap();
        assert_eq!(batch.warnings.len(), 1);
        assert!(
            read_if_changed(&path, |current| Ok(current == fingerprint))
                .unwrap()
                .is_none()
        );
        conn.execute("UPDATE message SET data='changed JSON'", [])
            .unwrap();
        assert!(
            read_if_changed(&path, |current| Ok(current == fingerprint))
                .unwrap()
                .is_some()
        );
        drop(conn);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn reads_synthetic_database_without_mutating_it() {
        let db = std::env::temp_dir().join(format!(
            "cclens-opencode-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch("CREATE TABLE session(id TEXT, directory TEXT, parent_id TEXT, time_created INTEGER); CREATE TABLE message(id TEXT, session_id TEXT, data TEXT); CREATE TABLE part(id TEXT, message_id TEXT, session_id TEXT, data TEXT);") .unwrap();
        conn.execute("INSERT INTO session VALUES('s','/work',NULL,1000)", [])
            .unwrap();
        conn.execute("INSERT INTO session VALUES('fork','/work', 's',2000)", [])
            .unwrap();
        conn.execute(
            "INSERT INTO session VALUES('fork-no-parent','/work',NULL,3000)",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO message VALUES(?1,?2,?3)", params!["m-copy-no-parent", "fork-no-parent", r#"{"role":"assistant","time":{"created":1500},"modelID":"m1","cost":0,"tokens":{"total":0,"input":10,"output":4,"reasoning":1,"cache":{"read":2,"write":3}}}"#]).unwrap();
        conn.execute("INSERT INTO message VALUES(?1,?2,?3)", params!["m", "s", r#"{"role":"assistant","time":{"created":1500},"modelID":"m1","cost":0,"tokens":{"total":0,"input":10,"output":4,"reasoning":1,"cache":{"read":2,"write":3}}}"#]).unwrap();
        conn.execute("INSERT INTO message VALUES(?1,?2,?3)", params!["m-copy", "fork", r#"{"role":"assistant","time":{"created":1500},"modelID":"m1","cost":0,"tokens":{"total":0,"input":10,"output":4,"reasoning":1,"cache":{"read":2,"write":3}}}"#]).unwrap();
        conn.execute("INSERT INTO message VALUES(?1,?2,?3)", params!["m-new", "fork", r#"{"role":"assistant","time":{"created":2500},"modelID":"m1","cost":0.01,"tokens":{"total":3,"input":2,"output":1,"reasoning":0,"cache":{"read":0,"write":0}}}"#]).unwrap();
        conn.execute(
            "INSERT INTO message VALUES(?1,?2,?3)",
            params!["u", "s", r#"{"role":"user"}"#],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO part VALUES(?1,?2,?3,?4)",
            params![
                "p",
                "u",
                "s",
                r#"{"type":"text","text":"synthetic prompt"}"#
            ],
        )
        .unwrap();
        conn.execute("INSERT INTO part VALUES(?1,?2,?3,?4)", params!["t", "m", "s", r#"{"type":"tool","tool":"edit","state":{"status":"error","error":"String to replace not found in file","input":{"file_path":"a.rs"}}}"#]).unwrap();
        conn.execute("INSERT INTO part VALUES(?1,?2,?3,?4)", params!["e", "m", "s", r#"{"type":"tool","tool":"edit","state":{"status":"completed","input":{"file_path":"a.rs"}}}"#]).unwrap();
        drop(conn);
        let before = std::fs::metadata(&db).unwrap().len();
        let (_, batch) = read_if_changed(&db, |_| Ok(false)).unwrap().unwrap();
        assert_eq!(batch.sessions.len(), 3);
        assert_eq!(batch.events.iter().filter(|e| e.kind == "usage").count(), 2);
        let usage = batch.events.iter().find(|e| e.kind == "usage").unwrap();
        assert_eq!(usage.tokens.input, Some(15));
        assert_eq!(usage.tokens.output, Some(5));
        assert_eq!(usage.tokens.total, Some(20));
        assert_eq!(usage.api_cost_usd, Some(0.0));
        assert_eq!(usage.evidence.row_id.as_deref(), Some("m"));
        assert_eq!(
            batch.events.iter().filter(|e| e.kind == "prompt").count(),
            1
        );
        assert!(
            batch
                .events
                .iter()
                .any(|e| e.kind == "failure" && e.category.is_some())
        );
        assert!(
            batch
                .events
                .iter()
                .any(|e| e.kind == "edit" && e.target.as_deref() == Some("a.rs"))
        );
        assert_eq!(std::fs::metadata(&db).unwrap().len(), before);
        std::fs::remove_file(db).unwrap();
    }
}
