use cclens::{adapter, multi::Batch};
use serde_json::{Value, json};

#[test]
fn machine_context_is_not_user_intervention_across_jsonl_adapters() {
    let injected = [
        "",
        "  \n\t",
        "# AGENTS.md instructions for /synthetic\n\n<INSTRUCTIONS>nope</INSTRUCTIONS>",
        "  <permissions instructions>nope</permissions instructions>",
        "<environment_context>nope</environment_context>",
        "<recommended_plugins>nope</recommended_plugins>",
        "<turn_aborted>nope</turn_aborted>",
        "<subagent_notification>nope</subagent_notification>",
        "<task-notification>nope</task-notification>",
        "<external_codex_apps_open_page>nope</external_codex_apps_open_page>",
        "[Request interrupted by user for tool use]",
        "This session is being continued from a previous conversation that ran out of context.\nSummary: nope",
    ];
    let human = [
        "いや、戻して",
        "この <task-notification> の扱いは間違っている",
        "```text\n<permissions instructions>nope\n``` を説明して",
    ];
    for agent in ["claude", "codex", "pi", "omp"] {
        let mut records = match agent {
            "codex" => vec![json!({"type":"session_meta","payload":{"id":"s","cwd":"/synthetic"}})],
            "pi" | "omp" => vec![json!({"type":"session","id":"s","cwd":"/synthetic"})],
            _ => Vec::new(),
        };
        for (index, text) in injected.iter().chain(human.iter()).enumerate() {
            let id = format!("p{index}");
            records.push(match agent {
                "claude" => json!({"type":"user","uuid":id,"sessionId":"s","message":{"role":"user","content":text}}),
                "codex" => json!({"type":"response_item","payload":{"id":id,"type":"message","role":"user","content":[{"type":"input_text","text":text}]}}),
                _ => json!({"type":"message","id":id,"message":{"role":"user","content":[{"type":"text","text":text}]}}),
            });
        }
        let text = records
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let batch: Batch = match agent {
            "claude" => adapter::claude::parse(&text, "synthetic.jsonl"),
            "codex" => adapter::codex::parse(&text, "synthetic.jsonl"),
            _ => adapter::pi::parse(&text, "synthetic.jsonl"),
        };
        let prompts: Vec<_> = batch.events.iter().filter(|e| e.kind == "prompt").collect();
        assert_eq!(prompts.len(), human.len(), "{agent}: {prompts:?}");
        assert_eq!(prompts[0].category.as_deref(), Some("correct"), "{agent}");
        assert_eq!(
            prompts[0].evidence.line,
            Some(records.len() - human.len() + 1)
        );
    }
}

#[test]
fn edit_density_is_not_reported_as_proof_of_failed_repairs() {
    let events = (0..3)
        .map(|i| {
            (
                "pi".into(),
                None,
                cclens::multi::Event {
                    id: format!("edit-{i}"),
                    kind: "edit".into(),
                    session_id: "s".into(),
                    target: Some("/synthetic/a.rs".into()),
                    timestamp_ms: Some(1_000 + i * 1_000),
                    ..Default::default()
                },
            )
        })
        .collect::<Vec<_>>();
    let report = cclens::multi::analysis::stuck(&events, 300, 3);
    assert_eq!(report["rows"].as_array().unwrap().len(), 1);
    assert_eq!(report["signal"], "edit_density");
    assert_eq!(report["proves_failed_repair"], false);
}
