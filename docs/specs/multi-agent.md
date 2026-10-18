# Local multi-agent analysis

The installable binary is `cclens-multi`; the upstream library and Git history
remain intact. CLI execution never creates repositories, publishes data, or
edits agent configuration. Repository setup is separate from analysis.
`claude` forwards to the original read-only reports.

Agent adapters normalize inputs into sessions and evidence-bearing events,
including runtime-specific identity rules applied once by each adapter. The CLI
does not interpret event IDs.
Common aggregation and catalog/trace joins are pure transforms in
`multi::analysis`; they reuse upstream failure, prompt, and edit-burst classifiers.
The same private SQLite file holds both `multi_*` and original Claude tables.

Token input includes cache reads and cache writes. Those cache counters and
reasoning output are subsets, never added to totals a second time. Unknown
counters stay NULL. Report sums are sums of observed counters with coverage,
not claims of complete billing. Logged API-equivalent cost is separate from
actual payment, which is unavailable. No price table or network lookup is used.

Claude response IDs, Codex response IDs or legacy cumulative boundaries,
OpenCode message/part IDs, and pi entry IDs plus timestamps identify events.
Short pi IDs alone do not identify independent sessions. Copied fork histories are deduplicated
across an agent, not just within a file. Codex cumulative baselines in fork
prefixes do not represent new usage. Sources are replaced transactionally on
refresh; unchanged files may be skipped only under the same adapter version.
JSONL change detection hashes bytes with bounded buffering rather than retaining
the whole file; changed files are then read and their content fingerprint is
recomputed before replacement. OpenCode hashes the raw session/message/part rows
inside the same read transaction used for extraction, before parsing JSON or
classifying events. Neither check relies on file size or modification time, so
same-size rewrites and committed WAL changes remain visible.
Malformed lines, missing counters, inaccessible sources and unrecognized
records are reported as coverage gaps. A missing source is not zero usage.
Claude bookkeeping records that carry no usage, intent or tool outcome (tool
progress, subagent start/result, summaries, titles, API request shapes and
blobs) are recognized and skipped,
not counted as unknown (see the known-type list in `src/adapter/claude.rs`).

Codex response usage takes precedence from its first timestamp onward; an older
cumulative prefix is retained with estimated identity. OpenCode forks remap IDs
and may omit parent_id: exact message metadata predating session creation and
matching an earlier session is copied history. This is an inference, not fuzzy
text similarity. Indexing all messages before classification makes it independent
of insertion order. OpenCode output excludes reasoning in its source format;
normalization adds it once. See the upstream [session implementation](https://github.com/anomalyco/opencode/blob/dev/packages/opencode/src/session/session.ts).

Reports default to 50 rows, flag truncation, accept a maximum limit of 10,000,
and emit one JSON object. Date boundaries use UTC, with inclusive since and
exclusive until. `analyze --dry-run` does not create a DB; `--frozen` opens it
read-only. CLI validation precedes writes. Exit codes are 0 for success with
explicit coverage gaps, 1 for runtime/input errors, and 2 for invalid arguments.
Usage, failure and prompt reports stream filtered normalized events from SQLite
into per-group accumulators, retaining only counters and at most three evidence
examples per group. All sections of a report share one read snapshot, including
doctor's separate per-kind queries. Usage sums remain checked unsigned sums with per-counter
coverage; overflowing sums are NULL. Edit-burst detection still needs the matching
edit timeline. Row limits bound output, not the events needed for exact totals.

Input JSONL files and the OpenCode database are opened read-only. Stored multi_* facts
contain counters, classifications and source references, never prompt bodies,
command bodies, configuration bodies or authentication material. The upstream
Claude tables retain their existing bounded error excerpts. Evidence
references use one-based JSONL lines or SQLite table and row IDs.

Runtimes can write machine context as user-role text. The shared adapter prompt
boundary excludes empty text and known leading markers for harness instructions
(`AGENTS.md`, permissions, environment and plugin context), abort/subagent/task
notifications, external-app notices, user-interruption notices and Claude
continuation summaries. Leading whitespace is ignored. Markers quoted inside a
human request or code fence do not suppress that request. This is a conservative
prefix heuristic, not proof of human authorship for all remaining text; core
prompt classification remains format-independent. Native metadata exclusions
(such as Claude `isMeta` and OpenCode synthetic/ignored parts) still apply.
Refresh re-extracts cached sources after this change; `--frozen` reads the
previously extracted facts without reclassifying them.

A Codex command lifecycle record with explicit exit code 1 and empty output is not a failure when its command is a standalone `rg` search: ripgrep uses this status for no matches. Recognition is conservative (direct argv, or a simple `sh` / `bash` / `zsh` `-c` / `-lc` invocation). Shell comments, composition, redirection, expansion, missing fields, nonempty output and other exit codes retain the existing failure behavior. Like other successful commands, no-match searches do not emit failure events. Refresh invalidates cached extraction from before this change; `--frozen` does not reinterpret an older store.

User classifications, error categories and edit bursts are heuristics. An edit
burst identifies repeated edits, not a proven unsuccessful repair. The stuck
report explicitly emits `signal: "edit_density"` and
`proves_failed_repair: false`; no failed-validation chain is inferred from edits
alone. Catalogued configuration is not evidence that the agent loaded it.
Inventory separates
whole-definition `static_tokens`, estimated `startup_tokens`, and
`on_demand_tokens`, with `load_mode` and `startup_measurement`. Claude catalog
entries preserve the existing adapter's cost and load-mode semantics. Other
runtimes' load modes and startup/on-demand costs remain NULL until their loading
contracts are known; file presence or size is not evidence of startup cost.
Skill reads and explicit invocations are reported separately; lack of traces
is never called unused. Global entries are shadowed by a same-agent, same-kind,
same-ID project entry for traces from that project, so one trace does not count
against both entries.

Acceptance checks cover five-agent synthetic input through CLI output,
incremental retry, cumulative resets, copied forks, missing fields, malformed
tail recovery, source safety, upstream Claude tests, and real local input.
Commands must terminate with closed stdin and work outside the checkout.

OMP is reported separately from pi using the shared session JSONL family, with
`--omp` overriding `~/.omp/agent/sessions`. Older stored roots without OMP remain
readable and acquire the default root on refresh. OMP reasoningTokens is an
output subset; developer context and credential/title/service metadata is never stored as content.
Known observation-pack ledger records are auxiliary, not session events: report
how many were excluded in source warnings, without calling them unknown session
records or counting their originalTokens as model usage. Unrecognized ledger
shapes still report unknown records. OpenCode's total counter is optional when
the component counters permit a derived total; malformed explicit counters
remain coverage warnings. Refresh invalidates previous adapter-version caches.
