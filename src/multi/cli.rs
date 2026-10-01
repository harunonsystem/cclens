//! Non-interactive CLI composition; reports operate only on the normalized store.
use super::{Event, store as db};
use crate::{adapter, store::Store};
use anyhow::{Context, Result, bail};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Agent {
    Claude,
    Codex,
    Opencode,
    Pi,
}
impl Agent {
    fn label(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Opencode => "opencode",
            Self::Pi => "pi",
        }
    }
}

#[derive(Parser)]
#[command(
    name = "cclens-multi",
    version,
    about = "Local, read-only analysis of Claude Code, Codex, OpenCode and pi logs."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
    /// Emit typed JSON only on stdout. Diagnostics go to stderr.
    #[arg(long, global = true)]
    json: bool,
    /// Private derived SQLite store. Default: $XDG_STATE_HOME/cclens-multi/analysis.db.
    #[arg(long, global = true)]
    db: Option<PathBuf>,
    /// Read the existing store without refreshing any inputs.
    #[arg(long, global = true)]
    frozen: bool,
    /// Preview analyze input roots and counts without writing a store.
    #[arg(long, global = true)]
    dry_run: bool,
    /// Limit report rows; default 50. Get uses the event ID to retrieve evidence.
    #[arg(long,global=true,default_value="50",value_parser=clap::value_parser!(u32).range(1..=10000))]
    limit: u32,
    #[arg(long, global = true)]
    agent: Option<Agent>,
    /// Exact recorded project path.
    #[arg(long, global = true)]
    project: Option<String>,
    /// Exact model name; OpenCode uses provider/model.
    #[arg(long, global = true)]
    model: Option<String>,
    /// Inclusive RFC3339 or YYYY-MM-DD boundary; date-only values use UTC.
    #[arg(long,global=true,value_parser=boundary)]
    since: Option<i64>,
    /// Exclusive RFC3339 or YYYY-MM-DD boundary; date-only values use UTC.
    #[arg(long,global=true,value_parser=boundary)]
    until: Option<i64>,
    /// Alternate home for source and configuration discovery.
    #[arg(long, global = true)]
    home: Option<PathBuf>,
    /// Claude transcript root, read-only.
    #[arg(long, global = true)]
    claude: Option<PathBuf>,
    /// Codex JSONL root, read-only.
    #[arg(long, global = true)]
    codex: Option<PathBuf>,
    /// OpenCode SQLite database, read-only, including its live WAL.
    #[arg(long, global = true)]
    opencode: Option<PathBuf>,
    /// pi JSONL root, read-only.
    #[arg(long, global = true)]
    pi: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Command {
    /// Refresh the four agent adapters and configuration catalog.
    Analyze,
    /// Token totals by agent, project, model and optional UTC time bucket.
    Usage {
        #[arg(long, value_enum)]
        by: Option<Period>,
    },
    /// Repeated tool failures with source evidence. Categories are estimated.
    Failures {
        #[arg(long,default_value="2",value_parser=clap::value_parser!(u32).range(1..))]
        min_count: u32,
    },
    /// Rapid repeated edits to one file. A burst is an estimated repair-loop signal.
    Stuck {
        #[arg(long,default_value="3",value_parser=clap::value_parser!(u32).range(2..))]
        min_edits: u32,
        #[arg(long,default_value="300",value_parser=clap::value_parser!(u32).range(1..))]
        gap_seconds: u32,
    },
    /// Estimated user intervention classes: instruct, correct, steer, question.
    Prompts,
    /// Configuration catalog and observed skill traces. Missing traces are unknown.
    Inventory,
    /// Source availability and bounded extraction warnings.
    Sources,
    /// Coverage, token totals, repeated failures and edit bursts.
    Doctor,
    /// Retrieve one normalized event and its original source pointer.
    Get { id: String },
    /// Print the compact command/output contract for an agent.
    AgentContext,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Period {
    Day,
    Week,
    Month,
    Year,
}

#[derive(Clone, Serialize, Deserialize)]
struct Roots {
    home: PathBuf,
    claude: PathBuf,
    codex: PathBuf,
    opencode: PathBuf,
    pi: PathBuf,
}
impl Roots {
    fn sources(&self) -> [(&str, &Path); 4] {
        [
            ("claude", &self.claude),
            ("codex", &self.codex),
            ("opencode", &self.opencode),
            ("pi", &self.pi),
        ]
    }
}

pub fn run() -> Result<()> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.get(1).is_some_and(|s| s == "claude") {
        return legacy(args);
    }
    let cli = Cli::parse();
    if matches!(cli.command, Command::AgentContext) {
        return emit(
            &json!({"schema_version":1,"binary":"cclens-multi","first_command":"cclens-multi doctor --json",
            "commands":["analyze","usage","failures","stuck","prompts","inventory","sources","doctor","get","agent-context","claude <upstream command>"],
            "agents":["claude","codex","opencode","pi"],"filters":["--agent","--project","--model","--since","--until","--limit"],
            "stdout":"data only","stderr":"diagnostics","exit_codes":{"0":"success, including reported coverage gaps","1":"runtime or source error","2":"invalid CLI arguments"},
            "privacy":"inputs read-only; only derived local DB is written; no authentication or network needed",
            "unknown":"NULL values and per-counter known-event counts; never treat gaps as zero",
            "evidence":"get <id> --agent <agent> --frozen --json; source line is one-based or SQLite table/row ID",
            "cost":"reported API-equivalent USD only; actual payment is always unavailable",
            "configuration":"no automatic edits; traces do not prove loading or non-use"}),
            true,
        );
    }
    let since = cli.since;
    let until = cli.until;
    if since.zip(until).is_some_and(|(a, b)| a >= b) {
        Cli::command()
            .error(
                clap::error::ErrorKind::ValueValidation,
                "--since must be earlier than --until",
            )
            .exit();
    }
    if matches!(cli.command, Command::Get { .. }) && cli.agent.is_none() {
        Cli::command()
            .error(
                clap::error::ErrorKind::MissingRequiredArgument,
                "get requires --agent claude|codex|opencode|pi",
            )
            .exit();
    }
    let path = database_path(cli.db.clone())?;
    if cli.dry_run {
        if !matches!(cli.command, Command::Analyze) {
            Cli::command()
                .error(
                    clap::error::ErrorKind::ArgumentConflict,
                    "--dry-run is supported only with analyze",
                )
                .exit();
        }
        let roots = resolve_roots(&cli, None)?;
        validate_paths(&path, &roots, &cli)?;
        let rows=roots.sources().iter().filter(|(a,_)|cli.agent.is_none_or(|selected|selected.label()==*a))
            .map(|(a,p)|Ok(json!({"agent":a,"path":p,"status":if p.exists(){"available"}else{"unavailable"},
                "files":if !p.exists(){None}else if *a=="opencode"{Some(1)}else{Some(walk(p)?.len())}}))).collect::<Result<Vec<_>>>()?;
        return emit(
            &json!({"schema_version":1,"dry_run":true,"would_write_db":path,"rows":rows}),
            cli.json,
        );
    }
    if cli.frozen && matches!(cli.command, Command::Analyze) {
        Cli::command()
            .error(
                clap::error::ErrorKind::ArgumentConflict,
                "analyze cannot use --frozen; use a report to read existing data",
            )
            .exit();
    }
    let store = if cli.frozen {
        Store::open_readonly(&path)
            .context("open existing store; run cclens-multi analyze first")?
    } else {
        let prior = if path.is_file() {
            Store::open_readonly(&path)
                .ok()
                .and_then(|s| s.meta("multi_roots").ok().flatten())
                .map(|s| serde_json::from_str::<Roots>(&s))
                .transpose()?
        } else {
            None
        };
        let roots = resolve_roots(&cli, prior)?;
        validate_paths(&path, &roots, &cli)?;
        let mut store = Store::open(&path)?;
        db::initialize(store.connection())?;
        let stats = analyze(&store, &roots, cli.agent)?;
        store.set_meta("multi_roots", &serde_json::to_string(&roots)?)?;
        if matches!(cli.command, Command::Analyze) {
            return emit(&stats, cli.json);
        }
        eprintln!(
            "store refreshed: {} sources parsed, {} unchanged",
            stats["parsed_sources"], stats["unchanged_sources"]
        );
        store
    };
    let filter = db::Filter {
        agent: cli.agent.map(Agent::label),
        project: cli.project.as_deref(),
        model: cli.model.as_deref(),
        since,
        until,
    };
    let sources = db::sources(store.connection(), filter.agent)?;
    let mut value = match cli.command {
        Command::Usage { by } => {
            usage(&db::events(store.connection(), &filter, Some("usage"))?, by)
        }
        Command::Failures { min_count } => failures(
            &db::events(store.connection(), &filter, Some("failure"))?,
            min_count,
        ),
        Command::Stuck {
            min_edits,
            gap_seconds,
        } => stuck(
            &db::events(store.connection(), &filter, Some("edit"))?,
            gap_seconds,
            min_edits,
        ),
        Command::Prompts => prompts(&db::events(store.connection(), &filter, Some("prompt"))?),
        Command::Inventory => inventory(&store, &filter)?,
        Command::Sources => json!({"rows":db::source_details(store.connection(),filter.agent)?}),
        Command::Doctor => {
            let all = db::events(store.connection(), &filter, None)?;
            let kinds = |k: &str| {
                all.iter()
                    .filter(|(_, _, e)| e.kind == k)
                    .cloned()
                    .collect::<Vec<_>>()
            };
            json!({"usage":usage(&kinds("usage"),None),"failures":failures(&kinds("failure"),2),"stuck":stuck(&kinds("edit"),300,3),"prompts":prompts(&kinds("prompt"))})
        }
        Command::Get { id } => {
            if filter.agent.is_none() {
                bail!("get requires --agent claude|codex|opencode|pi");
            }
            let Some((a, p, e)) = db::event_by_id(store.connection(), filter.agent.unwrap(), &id)?
            else {
                bail!("event ID not found; obtain id from failures/stuck/prompts --json");
            };
            json!({"agent":a,"project":p,"event":e})
        }
        Command::Analyze | Command::AgentContext => unreachable!(),
    };
    limit_rows(&mut value, cli.limit as usize);
    value["schema_version"] = json!(1);
    value["sources"] = json!(sources);
    value["notes"] = json!([
        "Only observed data is aggregated. NULL means unknown, not zero.",
        "Error categories, user intent, and repair-loop diagnoses are estimates.",
        "API-equivalent cost is not actual payment. Historical sources may no longer exist."
    ]);
    emit(&value, cli.json)
}

fn legacy(args: Vec<std::ffi::OsString>) -> Result<()> {
    let mut forwarded = vec![std::ffi::OsString::from("cclens-multi claude")];
    let rest = &args[2..];
    if rest.first().is_some_and(|s| s == "optimize") && !rest.iter().any(|s| s == "--print") {
        bail!("configuration changes are out of scope; claude optimize requires --print");
    }
    if !rest.iter().any(|s| s == "--help" || s == "-h") {
        let option = |name: &str| {
            rest.windows(2)
                .find(|pair| pair[0] == name)
                .map(|pair| PathBuf::from(&pair[1]))
                .or_else(|| {
                    rest.iter().find_map(|arg| {
                        arg.to_str()
                            .and_then(|s| s.strip_prefix(&format!("{name}=")))
                            .map(PathBuf::from)
                    })
                })
        };
        let mut guard = Cli::try_parse_from(["cclens-multi", "analyze"])?;
        guard.claude = option("--projects");
        validate_paths(
            &database_path(option("--db"))?,
            &resolve_roots(&guard, None)?,
            &guard,
        )?;
    }
    for arg in rest {
        if arg == "--json" {
            forwarded.extend(["--format".into(), "json".into()]);
        } else {
            forwarded.push(arg.clone());
        }
    }
    if !rest
        .iter()
        .any(|s| s == "--db" || s.to_string_lossy().starts_with("--db="))
    {
        forwarded.extend(["--db".into(), database_path(None)?.into_os_string()]);
    }
    crate::cli::run_from(forwarded)
}

fn boundary(s: &str) -> Result<i64> {
    if let Ok(d) = chrono::DateTime::parse_from_rfc3339(s) {
        return Ok(d.timestamp_millis());
    }
    chrono::NaiveDate::parse_from_str(s,"%Y-%m-%d").ok().and_then(|d|d.and_hms_opt(0,0,0)).map(|d|d.and_utc().timestamp_millis())
        .with_context(||format!("invalid date '{s}'; use YYYY-MM-DD (UTC) or RFC3339, e.g. 2026-01-01T00:00:00+09:00"))
}

fn absolute(path: PathBuf) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    };
    if path.exists() {
        return Ok(path.canonicalize()?);
    }
    let mut normalized = PathBuf::new();
    for c in path.components() {
        match c {
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            std::path::Component::CurDir => {}
            c => normalized.push(c.as_os_str()),
        }
    }
    let existing = normalized
        .ancestors()
        .find(|p| p.exists())
        .context("cannot resolve path ancestor")?;
    Ok(existing
        .canonicalize()?
        .join(normalized.strip_prefix(existing)?))
}

fn database_path(path: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = path {
        return absolute(path);
    }
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute());
    let base = match state {
        Some(p) => p,
        None => std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .context("set HOME/USERPROFILE to an absolute path, or pass --db")?
            .join(".local/state"),
    };
    absolute(base.join("cclens-multi/analysis.db"))
}

fn resolve_roots(cli: &Cli, prior: Option<Roots>) -> Result<Roots> {
    let home = absolute(
        cli.home
            .clone()
            .or_else(|| prior.as_ref().map(|r| r.home.clone()))
            .or_else(|| {
                std::env::var_os("HOME")
                    .or_else(|| std::env::var_os("USERPROFILE"))
                    .map(PathBuf::from)
            })
            .context("set HOME/USERPROFILE or pass --home")?,
    )?;
    let prior = if cli.home.is_some() { None } else { prior };
    let use_env = std::env::var_os("HOME").is_some_and(|current| Path::new(&current) == home);
    let claude_default = std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|_| use_env)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".claude"))
        .join("projects");
    let codex_default = std::env::var_os("CODEX_HOME")
        .filter(|_| use_env)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"))
        .join("sessions");
    Ok(Roots {
        claude: absolute(
            cli.claude
                .clone()
                .or_else(|| prior.as_ref().map(|r| r.claude.clone()))
                .unwrap_or(claude_default),
        )?,
        codex: absolute(
            cli.codex
                .clone()
                .or_else(|| prior.as_ref().map(|r| r.codex.clone()))
                .unwrap_or(codex_default),
        )?,
        opencode: absolute(
            cli.opencode
                .clone()
                .or_else(|| prior.as_ref().map(|r| r.opencode.clone()))
                .unwrap_or_else(|| {
                    std::env::var_os("XDG_DATA_HOME")
                        .filter(|_| use_env)
                        .map(PathBuf::from)
                        .filter(|p| p.is_absolute())
                        .unwrap_or_else(|| home.join(".local/share"))
                        .join("opencode/opencode.db")
                }),
        )?,
        pi: absolute(
            cli.pi
                .clone()
                .or_else(|| prior.as_ref().map(|r| r.pi.clone()))
                .unwrap_or_else(|| home.join(".pi/agent/sessions")),
        )?,
        home,
    })
}

fn validate_paths(database: &Path, roots: &Roots, cli: &Cli) -> Result<()> {
    for (agent, path) in roots.sources() {
        if database == path || (path.is_dir() && database.starts_with(path)) {
            bail!(
                "--db must be outside input sources; refusing {}",
                database.display()
            );
        }
        let explicit = match agent {
            "claude" => cli.claude.is_some(),
            "codex" => cli.codex.is_some(),
            "opencode" => cli.opencode.is_some(),
            _ => cli.pi.is_some(),
        };
        if explicit && !path.exists() {
            bail!(
                "{agent} input does not exist: {}; correct the source flag",
                path.display()
            );
        }
    }
    Ok(())
}

fn walk(path: &Path) -> Result<Vec<PathBuf>> {
    if path.is_file() {
        return Ok(vec![path.to_owned()]);
    }
    let mut found = Vec::new();
    let mut dirs = vec![path.to_owned()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir)
            .with_context(|| format!("read source directory {}", dir.display()))?
        {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                dirs.push(entry.path());
            } else if kind.is_file() && entry.path().extension().is_some_and(|e| e == "jsonl") {
                found.push(entry.path());
            }
        }
    }
    found.sort();
    Ok(found)
}

fn analyze(store: &Store, roots: &Roots, selected: Option<Agent>) -> Result<Value> {
    let conn = store.connection();
    let mut parsed = 0;
    let mut skipped = 0;
    let mut available = 0;
    for (agent, root) in roots.sources() {
        if selected.is_some_and(|a| a.label() != agent) {
            continue;
        }
        let mut stmt =
            conn.prepare("SELECT path FROM multi_sources WHERE agent=?1 AND status='available'")?;
        let old = stmt
            .query_map([agent], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for source in old {
            if !Path::new(&source).exists() {
                db::source(
                    conn,
                    agent,
                    &source,
                    None,
                    "unavailable",
                    &["source disappeared; retained historical facts".into()],
                    0,
                )?;
            }
        }
        let root_text = root.display().to_string();
        if !root.exists() {
            db::source(
                conn,
                agent,
                &root_text,
                None,
                "unavailable",
                &["input source is absent; usage is unknown".into()],
                0,
            )?;
            continue;
        }
        let paths = if agent == "opencode" {
            vec![root.to_owned()]
        } else {
            walk(root)?
        };
        if paths.is_empty() {
            db::source(
                conn,
                agent,
                &root_text,
                None,
                "empty",
                &["no JSONL files found; historical usage is unknown".into()],
                0,
            )?;
        }
        for path in paths {
            let source = path.display().to_string();
            available += 1;
            let (fingerprint, mut batch) = if agent == "opencode" {
                let batch = adapter::opencode::read(&path)?;
                let fp = adapter::multi::fingerprint(&serde_json::to_string(&batch)?);
                (fp, batch)
            } else {
                let text = std::fs::read_to_string(&path)
                    .with_context(|| format!("read {agent} source {}", path.display()))?;
                let fp = adapter::multi::fingerprint(&text);
                if db::unchanged(conn, agent, &source, &fp)? {
                    skipped += 1;
                    continue;
                }
                let b = match agent {
                    "claude" => adapter::claude::parse(&text, &source),
                    "codex" => adapter::codex::parse(&text, &source),
                    _ => adapter::pi::parse(&text, &source),
                };
                (fp, b)
            };
            if db::unchanged(conn, agent, &source, &fingerprint)? {
                skipped += 1;
                continue;
            }
            for e in &mut batch.events {
                // pi uses short random entry ids; timestamp disambiguates independent sessions
                // while retaining the identity of copied branch prefixes.
                if agent == "pi" {
                    e.id = format!(
                        "{}:{}",
                        e.id,
                        e.timestamp_ms
                            .map(|t| t.to_string())
                            .unwrap_or_else(|| e.session_id.clone())
                    );
                }
                // Include event kind and target so a multi-file patch remains several edits.
                e.id = format!(
                    "{}:{}{}",
                    e.kind,
                    e.id,
                    e.target
                        .as_ref()
                        .map(|s| format!(":{}", adapter::multi::fingerprint(s)))
                        .unwrap_or_default()
                );
                if e.kind == "edit"
                    && let Some(target) = &e.target
                    && !Path::new(target).is_absolute()
                    && let Some(project) = batch
                        .sessions
                        .iter()
                        .find(|s| s.id == e.session_id)
                        .and_then(|s| s.project.as_deref())
                {
                    e.target = Some(Path::new(project).join(target).display().to_string());
                }
            }
            if !batch.warnings.is_empty() {
                eprintln!(
                    "{agent}: {} source warning(s) in {}",
                    batch.warnings.len(),
                    path.display()
                );
            }
            db::replace(conn, agent, &source, &fingerprint, &batch)?;
            parsed += 1;
        }
        let projects=conn.prepare("SELECT DISTINCT project FROM multi_sessions WHERE agent=?1 AND project IS NOT NULL")?
            .query_map([agent],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let catalog = adapter::config_multi::read(agent, &roots.home, &projects)?;
        db::save_inventory(conn, agent, &catalog)?;
    }
    if available == 0 {
        bail!(
            "no readable sources; set --claude/--codex/--opencode/--pi or --home; missing sources are unknown"
        );
    }
    let gaps: u64 = conn.query_row("SELECT COUNT(*) FROM multi_sources WHERE (?1 IS NULL OR agent=?1) AND (status!='available' OR unknown_records>0 OR json_array_length(warnings)>0)",
        [selected.map(Agent::label)],|row|row.get(0))?;
    Ok(
        json!({"schema_version":1,"parsed_sources":parsed,"unchanged_sources":skipped,"sources_with_gaps":gaps,"sources":db::sources(conn,selected.map(Agent::label))?}),
    )
}

type Rows = [(String, Option<String>, Event)];
type UsageKey = (String, Option<String>, Option<String>, Option<String>);
type PromptKey = (String, Option<String>, Option<String>);

fn usage(events: &Rows, period: Option<Period>) -> Value {
    let mut groups: BTreeMap<UsageKey, Vec<&Event>> = BTreeMap::new();
    for (a, p, e) in events {
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
        groups
            .entry((a.clone(), p.clone(), e.model.clone(), bucket))
            .or_default()
            .push(e);
    }
    let rows:Vec<_>=groups.into_iter().map(|((a,p,m,b),events)|{
        let sum=|pick:fn(&Event)->Option<u64>| {let values:Vec<_>=events.iter().filter_map(|e|pick(e)).collect();
            (if values.is_empty(){None}else{values.iter().try_fold(0_u64,|sum,v|sum.checked_add(*v))},values.len())};
        let (i,ki)=sum(|e|e.tokens.input);let(o,ko)=sum(|e|e.tokens.output);let(r,kr)=sum(|e|e.tokens.cache_read);
        let(w,kw)=sum(|e|e.tokens.cache_write);let(t,kt)=sum(|e|e.tokens.total);let(reason,k_reason)=sum(|e|e.tokens.reasoning);
        let costs:Vec<_>=events.iter().filter_map(|e|e.api_cost_usd).collect();
        json!({"agent":a,"project":p,"model":m,"bucket":b,"usage_events":events.len(),
            "input_tokens":i,"output_tokens":o,"cache_read_tokens":r,"cache_write_tokens":w,"reasoning_tokens":reason,"total_tokens":t,
            "known_input_events":ki,"known_output_events":ko,"known_cache_read_events":kr,"known_cache_write_events":kw,"known_reasoning_events":k_reason,"known_total_events":kt,
            "reported_api_equivalent_usd":if costs.is_empty(){None}else{Some(costs.iter().sum::<f64>())},"known_cost_events":costs.len(),"actual_payment_usd":null,
            "measurement":if events.iter().all(|e|e.measurement=="unavailable"){"unavailable"}else if events.iter().any(|e|e.measurement=="estimated"){"estimated"}else{"measured"},
             "estimated_usage_events":events.iter().filter(|e|e.measurement=="estimated").count(),"unavailable_usage_events":events.iter().filter(|e|e.measurement=="unavailable").count(),"cost_measurement":if costs.is_empty(){"unavailable"}else{"estimated"},"cost_source":"logged_api_equivalent","payment_measurement":"unavailable","unknown_timestamp_events":events.iter().filter(|e|e.timestamp_ms.is_none()).count()})
    }).collect();
    json!({"rows":rows,"timezone":"UTC","input_includes_cache":true,"reasoning_is_output_subset":true})
}

fn evidence(a: &str, e: &Event) -> Value {
    json!({"agent":a,"id":e.id,"session_id":e.session_id,"timestamp_ms":e.timestamp_ms,"source":e.evidence})
}
fn failures(events: &Rows, min: u32) -> Value {
    let mut groups: BTreeMap<UsageKey, Vec<&Event>> = BTreeMap::new();
    for (a, p, e) in events {
        groups
            .entry((a.clone(), p.clone(), e.tool.clone(), e.category.clone()))
            .or_default()
            .push(e);
    }
    let mut rows:Vec<_>=groups.into_iter().filter(|(_,es)|es.len()>=min as usize).map(|((a,p,t,c),es)|json!({"agent":a,"project":p,"tool":t,"category":c,
        "count":es.len(),"failure_measurement":"measured","category_measurement":"estimated","examples":es.iter().take(3).map(|e|evidence(&a,e)).collect::<Vec<_>>()})).collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r["count"].as_u64().unwrap_or(0)));
    json!({"rows":rows})
}
fn stuck(events: &Rows, gap: u32, min: u32) -> Value {
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
fn prompts(events: &Rows) -> Value {
    let mut groups: BTreeMap<PromptKey, Vec<&Event>> = BTreeMap::new();
    for (a, p, e) in events {
        groups
            .entry((a.clone(), p.clone(), e.category.clone()))
            .or_default()
            .push(e);
    }
    let rows:Vec<_>=groups.into_iter().map(|((a,p,c),es)|json!({"agent":a,"project":p,"classification":c,"count":es.len(),"measurement":"estimated",
        "examples":es.iter().take(3).map(|e|evidence(&a,e)).collect::<Vec<_>>()})).collect();
    json!({"rows":rows})
}
fn inventory(store: &Store, filter: &db::Filter<'_>) -> Result<Value> {
    let traces = db::events(store.connection(), filter, Some("skill"))?;
    let mut rows = db::inventory(store.connection(), filter.agent)?;
    rows.retain(|r| {
        filter
            .project
            .is_none_or(|p| r["project"].as_str() == Some(p))
    });
    for row in &mut rows {
        let matches: Vec<_> = traces
            .iter()
            .filter(|(a, project, e)| {
                row["agent"] == *a
                    && e.target.as_deref() == row["id"].as_str()
                    && (row["project"].is_null() || row["project"].as_str() == project.as_deref())
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
    Ok(
        json!({"rows":rows,"observed_skill_traces":traces.len(),"note":"Observed trace counts are lower bounds. Installed or read does not prove loaded; absent traces do not prove unused."}),
    )
}
fn limit_rows(value: &mut Value, limit: usize) {
    if let Some(rows) = value.get_mut("rows").and_then(Value::as_array_mut) {
        let total = rows.len();
        rows.truncate(limit);
        value["total_rows"] = json!(total);
        value["truncated"] = json!(total > limit);
        if total > limit {
            value["hint"] =
                json!("increase --limit or narrow --agent/--project/--model/--since/--until");
        }
    }
    for key in ["usage", "failures", "stuck", "prompts"] {
        if let Some(child) = value.get_mut(key) {
            limit_rows(child, limit);
        }
    }
}
fn emit(value: &Value, json_output: bool) -> Result<()> {
    use std::io::Write;
    let text = if json_output {
        serde_json::to_string(value)?
    } else {
        serde_json::to_string_pretty(value)?
    };
    let mut stdout = std::io::stdout().lock();
    match writeln!(stdout, "{text}") {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(e) => Err(e.into()),
    }
}
