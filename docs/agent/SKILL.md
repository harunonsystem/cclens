---
name: cclens-multi
description: cclens-multi でローカルの Agent 利用量、反復失敗、修正ループ、ユーザー介入、設定・skill の根拠を調べる。
---

# cclens-multi

1. `cclens-multi doctor --json` を実行し、`sources` の取得範囲を確認する。初回の入力先確認には `analyze --dry-run --json` を使う。
2. 目的に合う `usage`・`failures`・`stuck`・`prompts`・`inventory` を選び、`--agent`・`--project`・`--model`・`--since`・`--until` で絞って `--frozen --json` で読む。入力更新が必要なら `analyze --json` を実行する。
3. `truncated` が true ならフィルタを絞るか `--limit` を増やす。既定 50、最大 10,000。欠損の詳細は `sources --agent AGENT --frozen --json`。
4. 診断の `examples[].id` を `get ID --agent AGENT --frozen --json` で取得し、JSONL の一始まり行番号または SQLite の table/row ID を根拠として報告する。

`null` は不明。トークンは取得済みカウンタの合計と `known_*_events` を併記する。cache は input の内数、reasoning は output の内数。`measurement` と各推定フラグを保ち、推定のユーザー意図・修正ループを事実として断定しない。料金は API 換算額で、実際の支払額は取得不可。

skill の呼び出し・読み取り・ロード状態を分ける。トレースがない状態は使用ゼロや未使用と呼ばない。本家の詳しい Claude 設定分析は `cclens-multi claude inventory --json` などで読む。

stdout は JSON、stderr は診断。終了コード 0 は完了（欠損は JSON に残る）、1 は入力・実行エラー、2 は CLI 引数エラー。stdin と認証は不要。詳細な引数は `agent-context` または `--help`。

操作範囲は入力の読み取りとローカル集計 DB の更新。設定変更機能はない。実ログ・DB・認証情報・実設定を Git に追加せず、リモート作成・push・公開はユーザーの明示承認がある場合だけ行う。この文書を読むだけでは skill のインストールは行わない。
