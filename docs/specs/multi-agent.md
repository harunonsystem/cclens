# Local multi-agent analysis

The installable binary is `cclens-multi`; the upstream library and Git history
remain intact. CLI execution never creates repositories, publishes data, or
edits agent configuration. Repository setup is separate from analysis.
`claude` forwards to the original read-only reports.

Agent adapters normalize inputs into sessions and evidence-bearing events.
Common aggregation reuses upstream failure, prompt, and edit-burst classifiers.
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
Malformed lines, missing counters, inaccessible sources and unrecognized
records are reported as coverage gaps. A missing source is not zero usage.

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

Input JSONL files and the OpenCode database are opened read-only. Stored multi_* facts
contain counters, classifications and source references, never prompt bodies,
command bodies, configuration bodies or authentication material. The upstream
Claude tables retain their existing bounded error excerpts. Evidence
references use one-based JSONL lines or SQLite table and row IDs.

User classifications, error categories and edit bursts are heuristics. An edit
burst identifies repeated edits, not a proven unsuccessful repair. Catalogued
configuration is not evidence that the agent loaded it. Skill reads and explicit
invocations are reported separately; lack of traces is never called unused.

Acceptance checks cover four-agent synthetic input through CLI output,
incremental retry, cumulative resets, copied forks, missing fields, malformed
tail recovery, source safety, upstream Claude tests, and real local input.
Commands must terminate with closed stdin and work outside the checkout.
