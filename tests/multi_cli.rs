use serde_json::{Value, json};
use std::{
    fs,
    process::{Command, Output},
    time::{SystemTime, UNIX_EPOCH},
};

fn cli(root: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cclens-multi"))
        .current_dir(std::env::temp_dir())
        .args(args)
        .arg("--db")
        .arg(root.join("analysis.db"))
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn four_agent_cli_is_idempotent_and_keeps_missing_counters_unknown() {
    let root = std::env::temp_dir().join(format!(
        "cclens-multi-test-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    for agent in ["claude", "codex", "pi"] {
        fs::create_dir_all(root.join(agent)).unwrap();
    }
    fs::write(root.join("claude/session.jsonl"), concat!(
        "{\"type\":\"assistant\",\"uuid\":\"u1\",\"sessionId\":\"s1\",\"cwd\":\"/tmp/demo\",\"timestamp\":\"2026-01-01T00:00:01Z\",\"message\":{\"id\":\"m1\",\"model\":\"claude-demo\",\"usage\":{\"input_tokens\":10,\"cache_read_input_tokens\":3,\"cache_creation_input_tokens\":2,\"output_tokens\":5}}}\n",
        "{\"type\":\"assistant\",\"uuid\":\"u2\",\"sessionId\":\"s1\",\"cwd\":\"/tmp/demo\",\"timestamp\":\"2026-01-01T00:00:02Z\",\"message\":{\"id\":\"m1\",\"model\":\"claude-demo\",\"usage\":{\"input_tokens\":10,\"cache_read_input_tokens\":3,\"cache_creation_input_tokens\":2,\"output_tokens\":5}}}\n"
    )).unwrap();
    let codex = [
        json!({"type":"session_meta","payload":{"id":"s2","cwd":"/tmp/demo"}}),
        json!({"type":"turn_context","payload":{"model":"codex-demo"}}),
        json!({"type":"token_usage_record","timestamp":"2026-01-01T00:00:02Z","payload":{"session_id":"s2","thread_id":"s2","turn_id":"t1","response_id":"r1","usage":{"input_tokens":20,"cached_input_tokens":4,"output_tokens":6,"total_tokens":26}}}),
    ];
    fs::write(
        root.join("codex/session.jsonl"),
        codex
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    fs::write(root.join("pi/session.jsonl"), [
        json!({"type":"session","id":"s3","cwd":"/tmp/demo","version":3}),
        json!({"type":"message","id":"p1","timestamp":"2026-01-01T00:00:03Z","message":{"role":"assistant","model":"pi-demo","usage":{"input":30,"cacheRead":0,"cacheWrite":0,"output":7,"totalTokens":37}}}),
        json!({"type":"message","id":"p2","timestamp":"2026-01-01T00:00:04Z","message":{"role":"assistant","model":"pi-demo"}}),
    ].iter().map(Value::to_string).collect::<Vec<_>>().join("\n")).unwrap();
    // Short pi IDs may collide across sessions, while copied prefixes keep timestamp too.
    fs::write(root.join("pi/independent.jsonl"), [
        json!({"type":"session","id":"independent","cwd":"/tmp/demo","version":3}),
        json!({"type":"message","id":"p1","timestamp":"2026-01-01T00:00:05Z","message":{"role":"assistant","model":"pi-demo","usage":{"input":1,"cacheRead":0,"cacheWrite":0,"output":8,"totalTokens":9}}}),
    ].iter().map(Value::to_string).collect::<Vec<_>>().join("\n")).unwrap();
    fs::copy(root.join("pi/session.jsonl"), root.join("pi/copy.jsonl")).unwrap();
    let conn = rusqlite::Connection::open(root.join("opencode.db")).unwrap();
    conn.execute_batch("CREATE TABLE session(id TEXT, directory TEXT, parent_id TEXT,time_created INTEGER); CREATE TABLE message(id TEXT,session_id TEXT,data TEXT); CREATE TABLE part(id TEXT,message_id TEXT,session_id TEXT,data TEXT);").unwrap();
    conn.execute(
        "INSERT INTO session VALUES('s4','/tmp/demo',NULL,1767225600000)",
        [],
    )
    .unwrap();
    conn.execute("INSERT INTO message VALUES('o1','s4',?1)", [json!({"role":"assistant","modelID":"oc-demo","providerID":"demo","time":{"created":1767225604000_i64},"tokens":{"input":40,"output":8,"reasoning":0,"cache":{"read":0,"write":0}},"cost":0.01}).to_string()]).unwrap();
    for i in 0..3 {
        conn.execute(
            "INSERT INTO message VALUES(?1,'s4',?2)",
            rusqlite::params![
                format!("user{i}"),
                json!({"role":"user","time":{"created":1767225605000_i64+i}}).to_string()
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO part VALUES(?1,?2,'s4',?3)",
            rusqlite::params![
                format!("text{i}"),
                format!("user{i}"),
                json!({"type":"text","text":"synthetic instruction"}).to_string()
            ],
        )
        .unwrap();
    }
    drop(conn);
    // Own paths so args never borrow temporary PathBuf values.
    let paths: Vec<_> = ["claude", "codex", "pi", "opencode.db"]
        .iter()
        .map(|p| root.join(p).to_string_lossy().into_owned())
        .collect();
    let home = root.to_string_lossy();
    let args = vec![
        "analyze",
        "--json",
        "--home",
        &home,
        "--claude",
        &paths[0],
        "--codex",
        &paths[1],
        "--pi",
        &paths[2],
        "--opencode",
        &paths[3],
    ];
    let mut preview_args = args.clone();
    preview_args.push("--dry-run");
    let preview = cli(&root, &preview_args);
    assert!(
        preview.status.success(),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    let _: Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert!(!root.join("analysis.db").exists());
    let mut coverage = None;
    for attempt in 0..3 {
        let out = cli(&root, &args);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let analysis: Value = serde_json::from_slice(&out.stdout).unwrap();
        if attempt > 0 {
            assert_eq!(analysis["parsed_sources"], 0);
            assert_eq!(coverage.as_ref(), Some(&analysis["sources_with_gaps"]));
        }
        coverage = Some(analysis["sources_with_gaps"].clone());
    }
    let out = cli(&root, &["usage", "--frozen", "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    let rows = report["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 4);
    assert_eq!(
        rows.iter()
            .map(|r| r["output_tokens"].as_u64().unwrap())
            .sum::<u64>(),
        34
    );
    assert!(
        rows.iter()
            .any(|r| r["agent"] == "pi" && r["usage_events"] == 3 && r["known_output_events"] == 2)
    );
    assert!(rows.iter().all(|r| r["actual_payment_usd"].is_null()));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn copied_rollouts_diagnostics_and_closed_stdin_errors_are_safe() {
    let root = std::env::temp_dir().join(format!(
        "cclens-evidence-test-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(root.join(".codex/sessions")).unwrap();
    let mut records = vec![
        json!({"type":"session_meta","payload":{"id":"thread","cwd":"/tmp/demo"}}),
        json!({"type":"turn_context","payload":{"model":"demo-model"}}),
        json!({"type":"response_item","timestamp":"2026-01-01T00:00:01Z","payload":{"type":"message","role":"user","id":"u1","content":[{"type":"input_text","text":"continue"}]}}),
        json!({"type":"response_item","timestamp":"2026-01-01T00:00:01Z","metadata":{"client_authored":false},"payload":{"type":"message","role":"user","id":"injected","content":[{"text":"PROMPT_SECRET"}]}}),
        json!({"type":"event_msg","timestamp":"2026-01-01T00:00:02Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"output_tokens":5,"total_tokens":105}}}}),
        json!({"type":"token_usage_record","timestamp":"2026-01-01T00:00:02Z","payload":{"response_id":"response","thread_id":"thread","usage":{"input_tokens":100,"output_tokens":5,"total_tokens":105}}}),
    ];
    for i in 0..3 {
        records.push(json!({"type":"event_msg","timestamp":format!("2026-01-01T00:00:{:02}Z",10+i),"payload":{"type":"item_completed","item":{"type":"FileChange","id":format!("edit{i}"),"status":"completed","changes":{"src/main.rs":{"type":"update","content":"SOURCE_SECRET"}}}}}));
    }
    for i in 0..2 {
        records.push(json!({"type":"event_msg","timestamp":format!("2026-01-01T00:00:{:02}Z",20+i),"payload":{"type":"item_completed","item":{"type":"CommandExecution","id":format!("command{i}"),"status":"failed","exit_code":1,"command":"SECRET_COMMAND","aggregated_output":"command not found: demo"}}}));
    }
    let text = records
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(
        root.join(".codex/sessions/parent.jsonl"),
        format!("{text}\ninvalid tail"),
    )
    .unwrap();
    fs::write(root.join(".codex/sessions/copy.jsonl"), &text).unwrap();
    let home = root.to_string_lossy();
    let out = cli(
        &root,
        &["analyze", "--agent", "codex", "--home", &home, "--json"],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let usage: Value =
        serde_json::from_slice(&cli(&root, &["usage", "--frozen", "--json"]).stdout).unwrap();
    assert_eq!(usage["rows"][0]["output_tokens"], 5);
    let fail: Value =
        serde_json::from_slice(&cli(&root, &["failures", "--frozen", "--json"]).stdout).unwrap();
    assert_eq!(fail["rows"][0]["count"], 2);
    let id = fail["rows"][0]["examples"][0]["id"].as_str().unwrap();
    let get = cli(
        &root,
        &["get", id, "--agent", "codex", "--frozen", "--json"],
    );
    assert!(get.status.success());
    let event: Value = serde_json::from_slice(&get.stdout).unwrap();
    assert_eq!(event["event"]["evidence"]["line"], 10);
    let stuck: Value =
        serde_json::from_slice(&cli(&root, &["stuck", "--frozen", "--json"]).stdout).unwrap();
    assert_eq!(stuck["rows"][0]["edits"], 3);
    assert_eq!(stuck["rows"][0]["path"], "/tmp/demo/src/main.rs");
    let prompts: Value =
        serde_json::from_slice(&cli(&root, &["prompts", "--frozen", "--json"]).stdout).unwrap();
    assert_eq!(prompts["rows"][0]["classification"], "steer");
    assert_eq!(prompts["rows"][0]["count"], 1);
    let bytes = fs::read(root.join("analysis.db")).unwrap();
    for secret in ["PROMPT_SECRET", "SOURCE_SECRET", "SECRET_COMMAND"] {
        assert!(!String::from_utf8_lossy(&bytes).contains(secret));
    }
    for arguments in [
        vec!["get", "missing", "--json"],
        vec![
            "usage",
            "--since",
            "2026-01-02",
            "--until",
            "2026-01-01",
            "--json",
        ],
    ] {
        let invalid = cli(&root, &arguments);
        assert_eq!(invalid.status.code(), Some(2));
        assert!(invalid.stdout.is_empty());
        assert_eq!(fs::read(root.join("analysis.db")).unwrap(), bytes);
    }
    let invalid = cli(&root, &["usage", "--since", "wrong", "--json"]);
    assert_eq!(invalid.status.code(), Some(2));
    assert!(invalid.stdout.is_empty());
    let protected = Command::new(env!("CARGO_BIN_EXE_cclens-multi"))
        .args(["analyze", "--agent", "codex", "--home", &home, "--db"])
        .arg(root.join(".codex/sessions/parent.jsonl"))
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert_eq!(protected.status.code(), Some(1));
    assert_eq!(fs::read(root.join("analysis.db")).unwrap(), bytes);
    fs::write(root.join(".codex/sessions/parent.jsonl"), &text).unwrap();
    let out = cli(&root, &["analyze", "--agent", "codex", "--json"]);
    assert!(out.status.success());
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["sources_with_gaps"], 0);
    let filtered = cli(
        &root,
        &["usage", "--since", "2026-01-02", "--frozen", "--json"],
    );
    let filtered: Value = serde_json::from_slice(&filtered.stdout).unwrap();
    assert!(filtered["rows"].as_array().unwrap().is_empty());
    fs::remove_file(root.join(".codex/sessions/parent.jsonl")).unwrap();
    let result = cli(&root, &["analyze", "--agent", "codex", "--json"]);
    assert!(result.status.success());
    let analysis: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(analysis["sources_with_gaps"], 1);
    assert!(
        analysis["sources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|source| source["status"] == "unavailable")
    );
    let retained: Value =
        serde_json::from_slice(&cli(&root, &["usage", "--frozen", "--json"]).stdout).unwrap();
    assert_eq!(retained["rows"][0]["output_tokens"], 5);
    fs::remove_dir_all(root).unwrap();
}
