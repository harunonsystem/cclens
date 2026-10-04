//! Pure reports over normalized events. No input, database, clock or CLI access.
use super::Event;
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug)]
pub enum Period {
    Day,
    Week,
    Month,
    Year,
}

pub type Row = (String, Option<String>, Event);
pub type Rows = [Row];
type UsageKey = (String, Option<String>, Option<String>, Option<String>);
type PromptKey = (String, Option<String>, Option<String>);

#[derive(Default)]
struct UsageTotals {
    events: usize,
    sums: [Option<u64>; 6],
    known: [usize; 6],
    cost: f64,
    known_cost: usize,
    estimated: usize,
    unavailable: usize,
    unknown_timestamp: usize,
}

pub fn usage(
    events: impl IntoIterator<Item = Result<Row>>,
    period: Option<Period>,
) -> Result<Value> {
    let mut groups: BTreeMap<UsageKey, UsageTotals> = BTreeMap::new();
    for row in events {
        let (a, p, e) = row?;
        let bucket = period.and_then(|period| {
            e.timestamp_ms
                .and_then(chrono::DateTime::from_timestamp_millis)
                .map(|d| match period {
                    Period::Day => d.format("%Y-%m-%d").to_string(),
                    Period::Week => d.format("%G-W%V").to_string(),
                    Period::Month => d.format("%Y-%m").to_string(),
                    Period::Year => d.format("%Y").to_string(),
                })
        });
        let totals = groups.entry((a, p, e.model, bucket)).or_default();
        totals.events += 1;
        for (i, value) in [
            e.tokens.input,
            e.tokens.output,
            e.tokens.cache_read,
            e.tokens.cache_write,
            e.tokens.total,
            e.tokens.reasoning,
        ]
        .into_iter()
        .enumerate()
        {
            if let Some(value) = value {
                totals.sums[i] = if totals.known[i] == 0 {
                    Some(value)
                } else {
                    totals.sums[i].and_then(|sum| sum.checked_add(value))
                };
                totals.known[i] += 1;
            }
        }
        if let Some(cost) = e.api_cost_usd {
            totals.cost += cost;
            totals.known_cost += 1;
        }
        totals.estimated += usize::from(e.measurement == "estimated");
        totals.unavailable += usize::from(e.measurement == "unavailable");
        totals.unknown_timestamp += usize::from(e.timestamp_ms.is_none());
    }
    let rows:Vec<_>=groups.into_iter().map(|((a,p,m,b),totals)|{
        let [i,o,r,w,t,reason] = totals.sums;
        let [ki,ko,kr,kw,kt,k_reason] = totals.known;
        json!({"agent":a,"project":p,"model":m,"bucket":b,"usage_events":totals.events,
            "input_tokens":i,"output_tokens":o,"cache_read_tokens":r,"cache_write_tokens":w,"reasoning_tokens":reason,"total_tokens":t,
            "known_input_events":ki,"known_output_events":ko,"known_cache_read_events":kr,"known_cache_write_events":kw,"known_reasoning_events":k_reason,"known_total_events":kt,
            "reported_api_equivalent_usd":(totals.known_cost>0).then_some(totals.cost),"known_cost_events":totals.known_cost,"actual_payment_usd":null,
            "measurement":if totals.unavailable==totals.events{"unavailable"}else if totals.estimated>0{"estimated"}else{"measured"},
             "estimated_usage_events":totals.estimated,"unavailable_usage_events":totals.unavailable,"cost_measurement":if totals.known_cost==0{"unavailable"}else{"estimated"},"cost_source":"logged_api_equivalent","payment_measurement":"unavailable","unknown_timestamp_events":totals.unknown_timestamp})
    }).collect();
    Ok(
        json!({"rows":rows,"timezone":"UTC","input_includes_cache":true,"reasoning_is_output_subset":true}),
    )
}

fn evidence(a: &str, e: &Event) -> Value {
    json!({"agent":a,"id":e.id,"session_id":e.session_id,"timestamp_ms":e.timestamp_ms,"source":e.evidence})
}
pub fn failures(events: impl IntoIterator<Item = Result<Row>>, min: u32) -> Result<Value> {
    let mut groups: BTreeMap<UsageKey, (usize, Vec<Value>)> = BTreeMap::new();
    for row in events {
        let (a, p, e) = row?;
        let (count, examples) = groups
            .entry((a.clone(), p, e.tool.clone(), e.category.clone()))
            .or_default();
        *count += 1;
        if examples.len() < 3 {
            examples.push(evidence(&a, &e));
        }
    }
    let mut rows:Vec<_>=groups.into_iter().filter(|(_,(count,_))|*count>=min as usize).map(|((a,p,t,c),(count,examples))|json!({"agent":a,"project":p,"tool":t,"category":c,
        "count":count,"failure_measurement":"measured","category_measurement":"estimated","examples":examples})).collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r["count"].as_u64().unwrap_or(0)));
    Ok(json!({"rows":rows}))
}
pub fn stuck(events: &Rows, gap: u32, min: u32) -> Value {
    let edits: Vec<_> = events
        .iter()
        .filter_map(|(a, p, e)| {
            Some(crate::core::thrash::FileEdit {
                session_id: format!("{a}:{}", e.session_id),
                project: p.clone().unwrap_or_default(),
                path: e.target.clone()?,
                epoch: e.timestamp_ms? / 1000,
            })
        })
        .collect();
    let rows:Vec<_>=crate::core::thrash::detect_thrash(&edits,gap as i64,min).iter().map(|ep|{
        let (a,s)=ep.session_id.split_once(':').unwrap();
        let examples:Vec<_>=events.iter().filter(|(agent,_,e)|agent==a&&e.session_id==s&&e.target.as_deref()==Some(&ep.path)&&e.timestamp_ms.is_some_and(|t|t/1000>=ep.start_epoch&&t/1000<=ep.end_epoch)).take(3).map(|(_,_,e)|evidence(a,e)).collect();
        json!({"agent":a,"session_id":s,"project":if ep.project.is_empty(){None}else{Some(&ep.project)},"path":ep.path,"edits":ep.edits,
            "start_epoch":ep.start_epoch,"end_epoch":ep.end_epoch,"measurement":"estimated","examples":examples})
    }).collect();
    json!({"rows":rows,"unknown_timestamp_edits":events.iter().filter(|(_,_,e)|e.timestamp_ms.is_none()).count(),"gap_seconds":gap,"min_edits":min})
}
pub fn prompts(events: impl IntoIterator<Item = Result<Row>>) -> Result<Value> {
    let mut groups: BTreeMap<PromptKey, (usize, Vec<Value>)> = BTreeMap::new();
    for row in events {
        let (a, p, e) = row?;
        let (count, examples) = groups
            .entry((a.clone(), p, e.category.clone()))
            .or_default();
        *count += 1;
        if examples.len() < 3 {
            examples.push(evidence(&a, &e));
        }
    }
    let rows: Vec<_> = groups
        .into_iter()
        .map(|((a, p, c), (count, examples))| {
            json!({"agent":a,"project":p,"classification":c,"count":count,"measurement":"estimated",
        "examples":examples})
        })
        .collect();
    Ok(json!({"rows":rows}))
}
pub fn inventory(mut rows: Vec<Value>, traces: &Rows, project: Option<&str>) -> Value {
    let overrides: BTreeSet<_> = rows
        .iter()
        .filter(|r| r["kind"] == "skill")
        .filter_map(|r| {
            Some((
                r["agent"].as_str()?.to_owned(),
                r["id"].as_str()?.to_owned(),
                r["project"].as_str()?.to_owned(),
            ))
        })
        .collect();
    rows.retain(|r| project.is_none_or(|p| r["project"].as_str() == Some(p)));
    for row in &mut rows {
        let matches: Vec<_> = traces
            .iter()
            .filter(|(a, project, e)| {
                row["kind"] == "skill"
                    && row["agent"] == *a
                    && e.target.as_deref() == row["id"].as_str()
                    && (row["project"].as_str() == project.as_deref()
                        || (row["project"].is_null()
                            && !project.as_ref().is_some_and(|p| {
                                overrides.contains(&(
                                    a.clone(),
                                    e.target.clone().unwrap(),
                                    p.clone(),
                                ))
                            })))
            })
            .collect();
        let invocations = matches
            .iter()
            .filter(|(_, _, e)| e.category.as_deref() != Some("read"))
            .count();
        let reads = matches
            .iter()
            .filter(|(_, _, e)| e.category.as_deref() == Some("read"))
            .count();
        row["observed_invocations"] = json!((invocations > 0).then_some(invocations));
        row["observed_reads"] = json!((reads > 0).then_some(reads));
        row["usage_status"] = json!(if matches.is_empty() {
            "unavailable"
        } else {
            "observed_trace"
        });
        row["uses"] = Value::Null;
        row["loaded"] = Value::Null;
    }
    json!({"rows":rows,"observed_skill_traces":traces.len(),"note":"Observed trace counts are lower bounds. Installed or read does not prove loaded; absent traces do not prove unused."})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streamed_usage_keeps_unknowns_coverage_and_unsigned_overflow() {
        let events = [
            Event {
                tokens: super::super::Tokens {
                    input: Some(u64::MAX),
                    output: Some(3),
                    ..Default::default()
                },
                measurement: "estimated".into(),
                ..Default::default()
            },
            Event {
                tokens: super::super::Tokens {
                    input: Some(1),
                    ..Default::default()
                },
                measurement: "measured".into(),
                ..Default::default()
            },
            Event {
                measurement: "unavailable".into(),
                ..Default::default()
            },
        ]
        .into_iter()
        .map(|e| Ok(("claude".into(), None, e)));
        let report = usage(events, None).unwrap();
        let row = &report["rows"][0];
        assert_eq!(row["usage_events"], 3);
        assert!(row["input_tokens"].is_null());
        assert_eq!(row["known_input_events"], 2);
        assert_eq!(row["output_tokens"], 3);
        assert_eq!(row["known_output_events"], 1);
        assert!(row["cache_read_tokens"].is_null());
        assert_eq!(row["known_cache_read_events"], 0);
        assert_eq!(row["measurement"], "estimated");
        assert_eq!(row["unavailable_usage_events"], 1);
        assert_eq!(row["unknown_timestamp_events"], 3);
        assert!(usage(std::iter::once(Err(anyhow::anyhow!("read error"))), None).is_err());
    }

    #[test]
    fn inventory_attributes_traces_to_project_overrides_without_double_counting() {
        let catalog = vec![
            json!({"agent":"claude","kind":"skill","id":"review","project":null}),
            json!({"agent":"claude","kind":"skill","id":"review","project":"/tmp/example"}),
            json!({"agent":"claude","kind":"rule","id":"review","project":null}),
        ];
        let trace = |project: &str, category: &str| {
            (
                "claude".into(),
                Some(project.into()),
                Event {
                    kind: "skill".into(),
                    target: Some("review".into()),
                    category: Some(category.into()),
                    ..Default::default()
                },
            )
        };
        let traces = vec![trace("/tmp/example", "invoke"), trace("/tmp/other", "read")];
        let report = inventory(catalog, &traces, None);
        assert!(report["rows"][0]["observed_invocations"].is_null());
        assert_eq!(report["rows"][0]["observed_reads"], 1);
        assert_eq!(report["rows"][1]["observed_invocations"], 1);
        assert!(report["rows"][1]["observed_reads"].is_null());
        assert_eq!(report["rows"][2]["usage_status"], "unavailable");
    }
}
