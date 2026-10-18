---
paths:
  - "tests/**"
  - "**/fixtures/**"
  - "src/**/*.rs"
---

# Session Data Privacy — Synthetic Fixtures Only

Real Claude Code transcripts under `~/.claude/projects/` contain personal and
proprietary data: prompts, file contents, paths, project names, tool output.
Real Claude Code config is just as sensitive — `settings.json` can hold API
tokens and secrets, and `CLAUDE.md` / rules can carry private project context.
None of it — transcript or config — may enter the repository.

## Hard rules

- **Test fixtures are synthetic.** Author them by hand to exercise a specific
  rule. Never paste a real transcript — not even "lightly edited." If you need
  a realistic shape, construct a minimal record with invented ids, neutral
  paths (`/tmp/example`), and placeholder text.
- **Never commit harvested data or generated output.** Real `*.jsonl`
  transcripts, copied real config (`settings.json`, `CLAUDE.md`, skills, rules),
  the generated SQLite database, and any scratch export stay out of git.
  `.gitignore` enforces this, but do not defeat it with `git add -f`.
- **No real ids or paths in code or tests.** Session ids, agent ids, prompt
  ids, cwd slugs, and project names in fixtures must be fabricated.
- **Exploration happens against the live directory, read-only.** When verifying
  behavior against real data, read from `~/.claude/projects/` directly at
  runtime; never copy those files into the tree.

## Enforcement — the repository is public

Every push is published, so these rules are checked mechanically, not by
memory. The same checks run locally (`lefthook.yml`) and in CI (`privacy` job in
`.github/workflows/ci.yml`); CI is the gate hooks cannot skip.

- **Leaks** — `.betterleaks.toml` (betterleaks 2 defaults plus two rules)
  scans staged changes, commit messages, pushed commits, and in CI the whole
  history including commit messages. The rules flag a home directory with a real-looking user name
  (`/Users/<name>`, `/home/<name>`, `C:\Users\<name>`) and any email address.
- **Placeholders that pass** — use only these in fixtures, docs and code:
  home directories `user`, `me`, `you`, `example`, `runner`, `linuxbrew`,
  `metadata`, or a dot-directory (`/home/.claude`); emails at `example.com`,
  `example.org`, `example.net`, `*.invalid`, `*.test`, `localhost`, plus
  `noreply@` / `no-reply@` addresses and `git@` SSH remotes. Adding a
  placeholder means editing the rule's `filter` in `.betterleaks.toml` in the
  same change.
- **Commit identity** — `scripts/check-identity.sh` allows only the GitHub
  noreply address as author and committer (`noreply@github.com` as committer
  for merges made in the GitHub UI). Set it per clone with
  `git config user.email`. Commits taken from elsewhere (rebase, cherry-pick)
  keep their old author; pre-push and CI catch them.
- **Never name what is hidden.** Rules stay generic. Writing a real name,
  employer or domain into `.betterleaks.toml`, a test, or a commit message
  publishes the association the check exists to protect.
- **No bypass.** Do not use `--no-verify`, `LEFTHOOK=0`, or `betterleaks:allow`
  to get a finding past the gate; fix the content. A false positive is fixed by
  narrowing the rule, reviewed like any other change.

Session ids are not checked: a real id and a fabricated one look the same, and
`tests/` holds fabricated ones on purpose. Fabricating them stays a hard rule.

## Why

A privacy leak here is irreversible once pushed — transcripts may contain
secrets, customer data, or unpublished work. The cost of fabricating a fixture
is minutes; the cost of leaking one transcript is unbounded.
