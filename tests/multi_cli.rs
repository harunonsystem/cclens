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
fn inventory_preserves_startup_and_body_costs_without_guessing_other_runtimes() {
    let root = std::env::temp_dir().join(format!(
        "cclens-inventory-cost-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let content = "---\ndescription: abcd\n---\nsynthetic private body";
    for agent in ["claude", "codex"] {
        let dir = root.join(format!(".{agent}"));
        fs::create_dir_all(dir.join("skills/review")).unwrap();
        fs::write(dir.join("skills/review/SKILL.md"), content).unwrap();
    }
    fs::create_dir_all(root.join(".claude/projects")).unwrap();
    fs::write(root.join(".claude/projects/session.jsonl"),
        r#"{"type":"assistant","uuid":"synthetic","sessionId":"synthetic-session","message":{"id":"synthetic-message","usage":{"input_tokens":1,"output_tokens":1}}}"#
    ).unwrap();
    fs::create_dir_all(root.join(".codex/sessions")).unwrap();
    fs::write(
        root.join(".codex/sessions/session.jsonl"),
        r#"{"type":"session_meta","payload":{"id":"synthetic-codex"}}"#,
    )
    .unwrap();
    let home = root.to_string_lossy();
    let refreshed = cli(&root, &["inventory", "--home", &home, "--json"]);
    assert!(
        refreshed.status.success(),
        "{}",
        String::from_utf8_lossy(&refreshed.stderr)
    );
    let frozen = cli(&root, &["inventory", "--frozen", "--json"]);
    assert!(frozen.status.success());
    let report: Value = serde_json::from_slice(&frozen.stdout).unwrap();
    let rows = report["rows"].as_array().unwrap();
    let claude = rows
        .iter()
        .find(|r| r["agent"] == "claude" && r["id"] == "review")
        .unwrap();
    assert_eq!(claude["startup_tokens"], 1);
    assert_eq!(claude["static_tokens"], 12);
    assert_eq!(claude["load_mode"], "startup_description");
    assert_eq!(claude["startup_measurement"], "estimated");
    assert_eq!(claude["on_demand_tokens"], 12);
    let codex = rows
        .iter()
        .find(|r| r["agent"] == "codex" && r["id"] == "review")
        .unwrap();
    for field in ["startup_tokens", "on_demand_tokens", "load_mode"] {
        assert!(codex.get(field).unwrap().is_null());
    }
    assert_eq!(codex["startup_measurement"], "unavailable");
    assert!(!report.to_string().contains("synthetic private body"));
    fs::remove_dir_all(root).unwrap();
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
    let limited = cli(&root, &["usage", "--frozen", "--limit", "1", "--json"]);
    assert!(limited.status.success());
    let limited: Value = serde_json::from_slice(&limited.stdout).unwrap();
    assert_eq!(limited["total_rows"], 4);
    assert_eq!(limited["truncated"], true);
    assert_eq!(limited["rows"], json!([rows[0]]));
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
    let doctor = cli(&root, &["doctor", "--frozen", "--json"]);
    assert!(
        doctor.status.success(),
        "{}",
        String::from_utf8_lossy(&doctor.stderr)
    );
    let doctor: Value = serde_json::from_slice(&doctor.stdout).unwrap();
    for (kind, report) in [
        ("usage", &usage),
        ("failures", &fail),
        ("stuck", &stuck),
        ("prompts", &prompts),
    ] {
        assert_eq!(doctor[kind]["rows"], report["rows"]);
    }
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

#[test]
fn omp_usage_is_separate_idempotent_and_keeps_reasoning_and_credentials_safe() {
    let root = std::env::temp_dir().join(format!(
        "cclens-omp-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let sessions = root.join(".omp/agent/sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(sessions.join("session.jsonl"), [
        json!({"type":"session","id":"omp-synthetic","cwd":"/tmp/example","version":3}),
        json!({"type":"credential_pin","id":"pin","credential":"synthetic-secret"}),
        json!({"type":"title_change","id":"title","title":"synthetic"}),
        json!({"type":"message","id":"developer-context","message":{"role":"developer","content":"synthetic developer context"}}),
        json!({"type":"message","id":"usage","timestamp":"2026-01-01T00:00:00Z","message":{"role":"assistant","model":"synthetic-model","usage":{"input":10,"cacheRead":3,"cacheWrite":2,"output":7,"reasoningTokens":4,"totalTokens":22}}})
    ].iter().map(Value::to_string).collect::<Vec<_>>().join("\n")).unwrap();
    let before = fs::read(sessions.join("session.jsonl")).unwrap();
    let home = root.to_string_lossy();
    for _ in 0..2 {
        let result = cli(
            &root,
            &["usage", "--home", &home, "--agent", "omp", "--json"],
        );
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let data: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(data["rows"].as_array().unwrap().len(), 1);
        let row = &data["rows"][0];
        assert_eq!(row["agent"], "omp");
        assert_eq!(row["usage_events"], 1);
        assert_eq!(row["input_tokens"], 15);
        assert_eq!(row["output_tokens"], 7);
        assert_eq!(row["reasoning_tokens"], 4);
        assert_eq!(data["sources"][0]["unknown_records"], 0);
        let evidence = cli(
            &root,
            &[
                "get",
                "usage:usage:1767225600000",
                "--agent",
                "omp",
                "--frozen",
                "--json",
            ],
        );
        assert!(
            evidence.status.success(),
            "{}",
            String::from_utf8_lossy(&evidence.stderr)
        );
    }
    assert_eq!(before, fs::read(sessions.join("session.jsonl")).unwrap());
    assert!(
        !String::from_utf8_lossy(&fs::read(root.join("analysis.db")).unwrap())
            .contains("synthetic-secret")
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn old_roots_refresh_invalidates_pi_cache_and_discovers_omp_separately() {
    let root = std::env::temp_dir().join(format!(
        "cclens-old-roots-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let session = [
        json!({"type":"session","id":"synthetic-session","cwd":"/tmp/example","version":3}),
        json!({"type":"message","id":"a","timestamp":"2026-01-01T00:00:00Z","message":{"role":"assistant","model":"synthetic-model","usage":{"input":10,"cacheRead":3,"cacheWrite":2,"output":7,"reasoningTokens":4,"totalTokens":22}}}),
    ].iter().map(Value::to_string).collect::<Vec<_>>().join("\n");
    let pi = root.join(".pi/agent/sessions");
    fs::create_dir_all(&pi).unwrap();
    fs::write(pi.join("session.jsonl"), &session).unwrap();
    let initial = cli(
        &root,
        &[
            "analyze",
            "--home",
            &root.to_string_lossy(),
            "--agent",
            "pi",
            "--json",
        ],
    );
    assert!(initial.status.success());
    {
        let conn = rusqlite::Connection::open(root.join("analysis.db")).unwrap();
        conn.execute_batch(
            "UPDATE multi_sources SET version='multi-4', unknown_records=1;
            UPDATE multi_observations SET payload=json_set(payload,'$.tokens.reasoning',NULL);
            UPDATE meta SET value=json_remove(value,'$.omp') WHERE key='multi_roots';",
        )
        .unwrap();
    }
    let omp = root.join(".omp/agent");
    fs::create_dir_all(omp.join("sessions")).unwrap();
    fs::write(omp.join("sessions/session.jsonl"), session).unwrap();
    fs::create_dir_all(omp.join("skills/review")).unwrap();
    fs::write(
        omp.join("skills/review/SKILL.md"),
        "---\ndescription: synthetic\n---\nPrivate synthetic skill body",
    )
    .unwrap();
    let refreshed = cli(&root, &["analyze", "--json"]);
    assert!(
        refreshed.status.success(),
        "{}",
        String::from_utf8_lossy(&refreshed.stderr)
    );
    let refreshed: Value = serde_json::from_slice(&refreshed.stdout).unwrap();
    assert_eq!(refreshed["parsed_sources"], 2);
    let report = cli(&root, &["usage", "--frozen", "--json"]);
    assert!(report.status.success());
    let report: Value = serde_json::from_slice(&report.stdout).unwrap();
    let rows = report["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    for agent in ["pi", "omp"] {
        let row = rows.iter().find(|row| row["agent"] == agent).unwrap();
        assert_eq!(row["usage_events"], 1);
        assert_eq!(row["input_tokens"], 15);
        assert_eq!(row["reasoning_tokens"], 4);
    }
    let inventory = cli(
        &root,
        &["inventory", "--agent", "omp", "--frozen", "--json"],
    );
    assert!(inventory.status.success());
    let inventory: Value = serde_json::from_slice(&inventory.stdout).unwrap();
    assert!(
        inventory["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["agent"] == "omp" && row["id"] == "review")
    );
    assert!(
        !inventory
            .to_string()
            .contains("Private synthetic skill body")
    );
    fs::remove_dir_all(root).unwrap();
}
