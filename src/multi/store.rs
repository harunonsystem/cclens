//! Shared normalized SQLite storage, including source-level atomic replacement.
use super::{Batch, Event, analysis::Row};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

pub const VERSION: &str = "multi-6";

pub fn initialize(conn: &Connection) -> Result<()> {
    conn.busy_timeout(std::time::Duration::from_secs(10))?;
    conn.execute_batch("CREATE TABLE IF NOT EXISTS multi_sources (
        agent TEXT NOT NULL, path TEXT NOT NULL, fingerprint TEXT, version TEXT,
        status TEXT NOT NULL, warnings TEXT NOT NULL, unknown_records INTEGER NOT NULL,
        analyzed_at TEXT NOT NULL, PRIMARY KEY(agent,path));
        CREATE TABLE IF NOT EXISTS multi_sessions (
        agent TEXT NOT NULL, source TEXT NOT NULL, id TEXT NOT NULL, project TEXT,
        parent_id TEXT, started_ms INTEGER, PRIMARY KEY(agent,source,id));
        CREATE TABLE IF NOT EXISTS multi_observations (
        agent TEXT NOT NULL, source TEXT NOT NULL, id TEXT NOT NULL, session_id TEXT NOT NULL,
        timestamp_ms INTEGER, kind TEXT NOT NULL, model TEXT, payload TEXT NOT NULL,
        PRIMARY KEY(agent,source,id));
        CREATE INDEX IF NOT EXISTS multi_event_filter ON multi_observations(agent,kind,timestamp_ms);
        CREATE INDEX IF NOT EXISTS multi_identity ON multi_observations(agent,id,source);
        CREATE VIEW IF NOT EXISTS multi_events AS
        SELECT e.*,s.project,s.parent_id FROM multi_observations e
        LEFT JOIN multi_sessions s ON s.agent=e.agent AND s.source=e.source AND s.id=e.session_id
        WHERE e.source=(SELECT MIN(d.source) FROM multi_observations d WHERE d.agent=e.agent AND d.id=e.id);
        CREATE TABLE IF NOT EXISTS multi_inventory (
        agent TEXT NOT NULL,path TEXT NOT NULL,kind TEXT NOT NULL,id TEXT NOT NULL,
        payload TEXT NOT NULL,PRIMARY KEY(agent,path,kind,id));")?;
    Ok(())
}

pub fn unchanged(conn: &Connection, agent: &str, path: &str, fingerprint: &str) -> Result<bool> {
    Ok(conn.query_row("SELECT fingerprint=?3 AND version=?4 AND status='available' FROM multi_sources WHERE agent=?1 AND path=?2",
        params![agent,path,fingerprint,VERSION], |r| r.get(0)).optional()?.unwrap_or(false))
}

pub fn replace(
    conn: &Connection,
    agent: &str,
    path: &str,
    fingerprint: &str,
    batch: &Batch,
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM multi_observations WHERE agent=?1 AND source=?2",
        params![agent, path],
    )?;
    tx.execute(
        "DELETE FROM multi_sessions WHERE agent=?1 AND source=?2",
        params![agent, path],
    )?;
    {
        let mut stmt =
            tx.prepare("INSERT OR REPLACE INTO multi_sessions VALUES(?1,?2,?3,?4,?5,?6)")?;
        for s in &batch.sessions {
            stmt.execute(params![
                agent,
                path,
                s.id,
                s.project,
                s.parent_id,
                s.started_ms
            ])?;
        }
        let mut stmt = tx
            .prepare("INSERT OR REPLACE INTO multi_observations VALUES(?1,?2,?3,?4,?5,?6,?7,?8)")?;
        for e in &batch.events {
            stmt.execute(params![
                agent,
                path,
                e.id,
                e.session_id,
                e.timestamp_ms,
                e.kind,
                e.model,
                serde_json::to_string(e)?
            ])?;
        }
    }
    source(
        &tx,
        agent,
        path,
        Some(fingerprint),
        "available",
        &batch.warnings,
        batch.unknown_records,
    )?;
    tx.commit()?;
    Ok(())
}

pub fn source(
    conn: &Connection,
    agent: &str,
    path: &str,
    fingerprint: Option<&str>,
    status: &str,
    warnings: &[String],
    unknown: usize,
) -> Result<()> {
    conn.execute("INSERT INTO multi_sources VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
        ON CONFLICT(agent,path) DO UPDATE SET fingerprint=excluded.fingerprint,version=excluded.version,
        status=excluded.status,warnings=excluded.warnings,unknown_records=excluded.unknown_records,analyzed_at=excluded.analyzed_at",
        params![agent,path,fingerprint,VERSION,status,serde_json::to_string(warnings)?,unknown,chrono::Utc::now().to_rfc3339()])?;
    Ok(())
}

#[derive(Default)]
pub struct Filter<'a> {
    pub agent: Option<&'a str>,
    pub project: Option<&'a str>,
    pub model: Option<&'a str>,
    pub since: Option<i64>,
    pub until: Option<i64>,
}

pub fn events(
    conn: &Connection,
    filter: &Filter<'_>,
    kind: Option<&str>,
) -> Result<Vec<(String, Option<String>, Event)>> {
    with_events(conn, filter, kind, |events| events.collect())
}

pub fn with_events<T>(
    conn: &Connection,
    filter: &Filter<'_>,
    kind: Option<&str>,
    report: impl FnOnce(&mut dyn Iterator<Item = Result<Row>>) -> Result<T>,
) -> Result<T> {
    let mut stmt = conn.prepare(
        "SELECT agent,project,payload FROM multi_events
        WHERE (?1 IS NULL OR agent=?1) AND (?2 IS NULL OR project=?2)
        AND (?3 IS NULL OR model=?3) AND (?4 IS NULL OR timestamp_ms>=?4)
        AND (?5 IS NULL OR timestamp_ms<?5) AND (?6 IS NULL OR kind=?6)
        ORDER BY timestamp_ms,id,agent",
    )?;
    let rows = stmt.query_map(
        params![
            filter.agent,
            filter.project,
            filter.model,
            filter.since,
            filter.until,
            kind
        ],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, String>(2)?,
            ))
        },
    )?;
    let mut events = rows.map(|r| {
        let (a, p, e) = r?;
        Ok((a, p, serde_json::from_str(&e)?))
    });
    report(&mut events)
}

pub fn sources(conn: &Connection, agent: Option<&str>) -> Result<Vec<Value>> {
    let mut stmt = conn.prepare("SELECT agent,status,COUNT(*),SUM(unknown_records),SUM(json_array_length(warnings)),MAX(analyzed_at)
        FROM multi_sources WHERE (?1 IS NULL OR agent=?1) GROUP BY agent,status ORDER BY agent,status")?;
    let rows=stmt.query_map([agent],|r| Ok(serde_json::json!({"agent":r.get::<_,String>(0)?,"status":r.get::<_,String>(1)?,
        "sources":r.get::<_,u64>(2)?,"unknown_records":r.get::<_,u64>(3)?,"warnings":r.get::<_,u64>(4)?,"analyzed_at":r.get::<_,String>(5)?})))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn source_details(conn: &Connection, agent: Option<&str>) -> Result<Vec<Value>> {
    let mut stmt=conn.prepare("SELECT agent,path,status,warnings,unknown_records,analyzed_at FROM multi_sources WHERE (?1 IS NULL OR agent=?1) ORDER BY agent,path")?;
    let rows = stmt.query_map([agent], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, u64>(4)?,
            r.get::<_, String>(5)?,
        ))
    })?;
    rows.map(|row|{let(a,p,s,w,u,t)=row?;let warnings:Vec<String>=serde_json::from_str(&w)?;
        Ok(serde_json::json!({"agent":a,"path":p,"status":s,"unknown_records":u,"warning_count":warnings.len(),"warnings":warnings.into_iter().take(3).collect::<Vec<_>>(),"analyzed_at":t}))}).collect()
}

pub fn event_by_id(
    conn: &Connection,
    agent: &str,
    id: &str,
) -> Result<Option<(String, Option<String>, Event)>> {
    let row = conn
        .query_row(
            "SELECT project,payload FROM multi_events WHERE agent=?1 AND id=?2",
            params![agent, id],
            |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?;
    row.map(|(p, e)| Ok((agent.to_owned(), p, serde_json::from_str(&e)?)))
        .transpose()
}

pub fn save_inventory(conn: &Connection, agent: &str, rows: &[Value]) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM multi_inventory WHERE agent=?1", [agent])?;
    for row in rows {
        tx.execute(
            "INSERT OR REPLACE INTO multi_inventory VALUES(?1,?2,?3,?4,?5)",
            params![
                agent,
                row["path"].as_str(),
                row["kind"].as_str(),
                row["id"].as_str(),
                row.to_string()
            ],
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub fn inventory(conn: &Connection, agent: Option<&str>) -> Result<Vec<Value>> {
    let mut stmt = conn.prepare(
        "SELECT payload FROM multi_inventory WHERE (?1 IS NULL OR agent=?1) ORDER BY agent,path",
    )?;
    let rows = stmt.query_map([agent], |r| r.get::<_, String>(0))?;
    rows.map(|r| Ok(serde_json::from_str(&r?)?)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keeps_several_configuration_items_in_one_file() {
        let conn = Connection::open_in_memory().unwrap();
        initialize(&conn).unwrap();
        let rows = vec![
            serde_json::json!({"path":"/tmp/demo/mcp.json","kind":"mcp_server","id":"a"}),
            serde_json::json!({"path":"/tmp/demo/mcp.json","kind":"mcp_server","id":"b"}),
        ];
        save_inventory(&conn, "claude", &rows).unwrap();
        assert_eq!(inventory(&conn, Some("claude")).unwrap(), rows);
    }
}
