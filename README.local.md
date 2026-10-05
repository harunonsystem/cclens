# cclens-multi

Claude Code・Codex・OpenCode・pi・OMP のローカルログを共通 SQLite に集計する Rust CLI。コマンド名は `cclens-multi` で、既存の `cclens` と共存する。

本家 [lambdalisue/cclens](https://github.com/lambdalisue/cclens) の Git 履歴と [MIT LICENSE](LICENSE) を保持したローカル拡張。基点は `3df5f76eb14a53c4cb03d975fd4dbd4eb2f7cc70`。本家の説明は [README.md](README.md)、拡張の契約は [multi-agent spec](docs/specs/multi-agent.md) にある。

## 導入・別 PC への移動

1. GitHub の private repo にアクセスできる認証を用意し、`git clone https://github.com/harunonsystem/cclens-multi.git ~/sandbox/cclens-multi` を実行する。実ログと集計 DB のコピーは不要。
2. `~/sandbox/cclens-multi` で、Rust stable と Cargo を用意し、`cargo install --path . --locked` を実行する。この PC では Rust 1.98.1 で検証した。
3. `~/.cargo/bin` を PATH に入れ、ソース外で `cclens-multi agent-context` を実行する。`CARGO_INSTALL_ROOT` を指定した場合はその `bin/` を PATH に入れる。
4. `cclens-multi analyze --dry-run --json` で入力先を確認し、`cclens-multi doctor --json` を実行する。

インストール時には Cargo の依存取得が必要になる場合がある。分析時にはネットワーク・API キーを使わない。各 PC のログを各 PC の DB に集計する。複数 PC の DB を合算する機能は含めない。

## 使い方

```sh
cclens-multi analyze --json
cclens-multi usage --by month --since 2026-09-01 --until 2026-10-01 --json
cclens-multi failures --agent codex --min-count 2 --frozen --json
cclens-multi stuck --project /absolute/project/path --frozen --json
cclens-multi prompts --agent pi --frozen --json
cclens-multi inventory --frozen --json
cclens-multi sources --agent opencode --frozen --json
cclens-multi get '<example.id>' --agent codex --frozen --json
```

レポートは通常、増分分析してから出力する。`--frozen` は既存 DB の読み取りだけを行う。既定の上限は 50 行。`total_rows`・`truncated` を確認し、フィルタか `--limit`（最大 10,000）で絞る。`doctor` は利用量・失敗・編集反復・ユーザー介入をまとめる。Agent 向け手順は [docs/agent/SKILL.md](docs/agent/SKILL.md)、引数一覧は `--help`。

`--json` は stdout に単一の JSON を出力し、診断は stderr に出す。指定しない場合も整形済み JSON。stdin を使わず対話待ちは発生しない。終了コードは 0 が成功（取得範囲に欠損がある場合を含む）、1 が入力・DB・実行前提のエラー、2 が CLI の構文・引数のエラー。欠損は `sources` と各指標の取得件数で判定する。

## 入力と保存先

| Agent | 既定の入力 | 上書き |
| --- | --- | --- |
| Claude Code | `~/.claude/projects/` | `--claude PATH`、`CLAUDE_CONFIG_DIR` |
| Codex | `~/.codex/sessions/` | `--codex PATH`、`CODEX_HOME` |
| OpenCode | `~/.local/share/opencode/opencode.db` | `--opencode PATH`、`XDG_DATA_HOME` |
| pi | `~/.pi/agent/sessions/` | `--pi PATH` |

`--home PATH` は別の home 配下を使う。明示指定した入力ルートは DB に記録され、次の増分実行で再利用する。入力フラグはディレクトリに加えて単一 JSONL も受け付ける。OpenCode は SQLite とその WAL を読み取り専用のトランザクションで読む。

DB は `${XDG_STATE_HOME:-~/.local/state}/cclens-multi/analysis.db`。`--db /absolute/path/analysis.db` で変更できる。入力先への DB 書き込みは拒否する。Unix の保存ディレクトリと DB は所有者のみアクセス可能。消えた入力から抽出済みの履歴は保持し、ソースの不在を表示する。

## 指標の読み方

- トークンは入力に cache read/write、出力に reasoning を含む共通形式。cache と reasoning は内数なので重ねて加算しない。欠損は `null`、合計は取得できた値だけの合計。`known_*_events` と `usage_events` を比較する。
- Codex の旧累積カウンタは差分化し、重複スナップショットを除く。応答単位ログがある区間はそちらを優先する。旧区間の応答同一性は推定のため `estimated_usage_events` に入る。初期累積値の前史が不明な場合は直近応答だけを数え、警告する。
- 失敗の発生はログの error/終了コードを根拠にし、失敗カテゴリ、編集反復による修正ループ、指示・修正・継続・質問の分類は推定。編集反復だけでは作業の失敗を断定しない。`examples` → `get` で元の JSONL 行番号または SQLite の table/row ID を辿れる。
- `reported_api_equivalent_usd` はログに記録された API 換算額。モデル料金表から新しく計算しない。`actual_payment_usd` は常に `null`。定額契約の実際の支払額や OpenCode の provider 固有の単位と同一視しない。
- 設定・skill の存在と Markdown の概算トークン量を収集する。`static_tokens` は定義全体、`startup_tokens` は起動時、`on_demand_tokens` は必要時に読む本文の概算。Claude は既存 parser の `load_mode` を保持し、他の Agent のロード方式・起動時コストは推測せず `null` にする。外部 plugin の全設定や実際のロード状態は網羅しない。Claude 以外の設定 JSON/TOML はファイルのメタデータだけを読む。skill の明示呼び出し・読み取りは区別し、トレースがなければ `null` / `unavailable`。取得できない使用回数をゼロや未使用と扱わない。

変更確認はJSONLを固定サイズのバッファでハッシュし、OpenCodeは読み取りトランザクション内で生の行をハッシュする。サイズ・更新時刻だけでスキップせず、同サイズの書き換えやWAL内の更新も検出する。usage・failures・promptsはSQLiteから逐次集計する。`--limit` は表示件数だけを制限し、総数や合計の計算対象は減らさない。編集バーストの検出は引き続き対象の編集履歴をメモリに保持する。

未対応のイベント・壊れた行・欠損カウンタは `sources --json` と stderr に出る。保存形式の全バージョン互換は保証せず、解析可能な根拠の範囲を出力する。pi の補助ログなど session header のない JSONL も取得範囲の警告になる。

## 本家の Claude 分析

`cclens-multi claude <本家のコマンド>` で既存の設定分析を使える。DB は共通だが、互換コマンドは本家の Claude 用テーブルを別途増分更新する。

```sh
cclens-multi claude analyze --json
cclens-multi claude inventory --frozen --json
cclens-multi claude overhead --frozen --json
```

互換側の `--scope`・`--by`・`--format` は本家の仕様を保つ。`--json` は `--format json` の別名。`optimize` は `--print` を付けた提案文の出力だけを許可する。設定の自動変更は行わない。

## 開発・検証

```sh
cargo test --locked
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo build --release --locked
```

テストの入力は合成データだけ。実ログ、集計 DB、認証情報、実設定の本文を Git に追加しない。入力は読み取り専用で、`multi_*` テーブルにはカウンタ・分類・パス・イベント ID・根拠位置を保存し、本文を保存しない。互換側の本家テーブルは既存仕様に従って短いエラー抜粋も保持する。ローカル DB とレポートのパス・モデル名も私的な情報として扱う。

管理先は [harunonsystem/cclens-multi](https://github.com/harunonsystem/cclens-multi) の private repo。既定ブランチは `multi-agent`。この PC の checkout は `~/sandbox/cclens-multi`、`origin` は管理先、`upstream` は本家を指す。

本家の CI・release workflow は履歴とともに保持しているが、この private repo の GitHub Actions は無効にしている。導入と検証には上記の Cargo コマンドを使う。crates.io・Homebrew への公開は行わない。

OMP は `--agent omp` で個別集計し、既定は `~/.omp/agent/sessions`（`--omp PATH` で変更）を読みます。SoL-Pi の既知の補助ledgerは除外件数をsource警告に残し、モデル利用量へ加算しません。
