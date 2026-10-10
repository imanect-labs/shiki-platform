# tool_search 評価基盤

`tool_search`（遅延ツールを検索して読み込む・#511）の検索精度を、カタログ規模・検索方式・
言語・読み込み件数・LLM を含めた E2E で測る。**製品の順位付けそのもの**を測るため、BM25F は
Python で書き直さず、Rust の順位 CLI（`crates/agent-core/examples/tool_search_rank.rs`・
`agent_core::EvalCatalog`）を呼ぶ。

## 前提

- `uv`（Python 3.12）と Rust ツールチェイン（順位 CLI を `--release` でビルドする）
- 埋め込み・reranker: ingestion-worker（`/embed`・`/rerank`・製品と同じ Ruri v3 / cross-encoder）。
  既定はプレビュー環境（`TSEVAL_WORKER_URL`・LAN 内・認証なし）。手元の compose なら
  `TSEVAL_WORKER_URL=http://localhost:8000`。
- LLM（データ生成と E2E）: OpenAI 互換。既定は opencode Go の `qwen3.6-plus`
  （`TSEVAL_LLM_BASE_URL` / `TSEVAL_LLM_MODEL` / `TSEVAL_LLM_KEY_FILE`）。opencode Go は
  `x-opencode-session` ヘッダが必須（クライアントが付ける）。

## 手順

```bash
cd eval/tool-search
uv sync
uv run python -m tseval.gen_catalog   # 合成カタログ（45 サービス × 22 ツール・日英の説明）
uv run python -m tseval.gen_queries   # 評価クエリ（正解 148 件 × ja / en / ja_para / agent）
uv run python -m tseval.evaluate      # 検索精度（E1〜E5）→ results/e*.json
uv run python -m tseval.e2e           # LLM にツールを選ばせる E2E → results/e2e.json
uv run python -m tseval.skills        # skill の一覧方式 vs 検索方式 → results/skills_e2e.json
uv run python -m tseval.report        # 集計表 → results/summary.md
```

生成物と LLM・埋め込みの応答は `data/cache/` にキャッシュする（再実行で再生成しない）。
`data/` の生成済みデータ（カタログ・クエリ）はコミットしてあり、そのまま再評価できる。

## データ

| ファイル | 中身 |
|---|---|
| `data/catalog_real_snapshot.json` | 製品のツール定義のスナップショット（dev 構成の通常チャット・`defer_loading` が遅延になった 13 件が検索対象） |
| `data/catalog_synth.json` | 合成ツール 990 件（業務 SaaS 45 サービスの MCP 風ツール・日英の説明と引数） |
| `data/queries.json` | 評価クエリ 592 件（正解は 1 ツール。紛らわしい近縁ツールでは満たせない依頼に限定して生成） |
| `data/skills.json` / `data/skill_queries.json` | 合成 skill 493 件（25 分野）と依頼 60 件 |

クエリ種別:

- `ja` … 業務の日本語の依頼
- `en` … 英語の依頼
- `ja_para` … 日本語の言い換え（ツールの語を避けた口語・語彙のずれを測る）
- `agent` … `ja` の依頼から、エージェントが `tool_search` に渡しそうな短い検索語（実運用で
  クエリを書くのはモデル）

## 実験

| ID | 何を測るか |
|---|---|
| E1 growth | カタログがサービス単位で増えるときの精度（実ツール → 合成サービスを順に追加） |
| E2 fixed | 評価クエリを固定し、妨害ツールだけ増やしたときの劣化 |
| E3 lang | カタログ言語（ja/en）× クエリ種別 |
| E4 limit | 読み込み件数 k と recall・読み込む定義のトークン |
| E5 latency | 1 クエリの処理時間（BM25 / 埋め込み / rerank）と `tool_search` 説明の長さ |
| E2E | 全ツール提示 vs `tool_search`（BM25 / RRF）で、LLM が正解ツールを呼べた率・トークン |
| skills | skill の一覧方式（製品の先頭 50 件 / 全件）vs 検索方式 |

方式: `bm25`（製品の打ち切り前順位）/ `bm25_prod`（製品が実際に読み込む 5 件）/ `emb`（Ruri v3）/
`rrf`（bm25 と emb の RRF・k=60）/ `rrf_rerank`（rrf 上位 20 件を cross-encoder で並べ替え）。

## 限界

- 合成データは LLM 生成（生成と E2E が同じモデル）。依頼文は近縁ツールと区別できるよう制約して
  作り、抜き取りで目視確認しているが、実ユーザーの分布とは異なる。
- E2E は推論トークンを止めて回す（`enable_thinking=false`・実行時間のため）。
- 実ツールは dev 構成で配線された 13 件のみ（Office・shell 等の配線が必要なものは含まない）。
