# AI によるワークフロー作成の評価基盤（GAP-30）

AI（チャットの `emit_workflow`）が作るワークフロー（アプリ）が、保存できるか・実行して期待どおりになるかを測る。
設計は台帳 `docs/workflow/quality-campaign.md` の付録 A「GAP-30 … 設計 — 反証レビュー後の改訂版」（§0〜§14）。

**製品の経路を書き直さない。** 説明文と system は製品が Anthropic へ送った要求から取り込み、保存できるかは
その版のサーバーの `POST /workflows/validate` で、厳格な採点は HEAD の検証器そのもの
（`crates/workflow-engine/examples/ir_lint.rs`）で判定する。製品の規則を Python に写さない。

## 前提

- `uv`（Python 3.12）。venv は `eval/app-builder/.venv`（.gitignore 済み）。
- 実 LLM を使うときは、手元の中継（`~/.config/shiki-dev/anthropic-relay.py`・`127.0.0.1:8787`・台帳 §9.2）が
  動いていること。**本物の鍵を読むのは手元の中継だけ**で、ここ（`abeval`）は鍵を読まない・持たない・設定に
  置かせない（`eval.toml` に鍵の形の項目があると読み込みで止まる）。
- ir_lint: `cargo build -p shiki-workflow-engine --example ir_lint`（`target/debug/examples/ir_lint`）。

## 手順

```bash
cd eval/app-builder
uv sync --frozen
uv run ruff check .
uv run pytest -m "not server" -q      # サーバーも LLM も要らないテスト
```

ir_lint の使い方（標準入力の JSONL → 標準出力の JSONL・誤りがあっても終了コード 0）:

```bash
echo '{"id":"t1","ir":{...}}' | target/debug/examples/ir_lint
target/debug/examples/ir_lint --catalog   # ノードのカタログ（出力の形）と警告のコードの表
```

## 上限と台帳

| 項目 | 既定 | 強制する場所 |
| --- | --- | --- |
| モデルの許可リスト | `claude-haiku-5-5` | 記録・上限つきの中継（abeval.proxy・送る前） |
| 1 回の実行（`run_cap_usd`） | $10 | 同上 |
| 評価の累計（`eval_cap_usd`） | $35（月で区切らない） | 同上 |
| 外側の歯止め（`global_cap_usd`） | $95（2026-10-25 からの累計） | 手元の中継（ここでは表示だけ） |

- 評価の台帳は `results/spend.jsonl`（1 行 = 1 回の要求・`cost_micros` はマイクロ USD の整数）。上限の正本なので
  **消さない**（.gitignore 済み。`git clean -x` で消えるので注意）。壊れた行があれば読み込みで止まる。
- 手元の中継の台帳（`~/.local/state/shiki-campaign/anthropic-spend.jsonl`）は読むだけ（8787 へは繋がない）。
- 単価は `eval.toml` の `[pricing]`（台帳 §6.1）。段の境は「入力＋キャッシュの読み＋キャッシュの書き」が 100,000
  トークンを超えたら高い段（手元の中継の `cost_of` と同じ）。1 回分は切り上げの整数で数える。

## 要求の形

- `temperature`・`top_p`・`top_k` は送らない（Haiku 5.5 は 400 を返す。製品の Anthropic アダプタも送らない）。
  assistant の prefill もしない。
- Mode A は httpx で Anthropic の形式を直接組む（SDK は入れない。自動の再試行と勝手に付くヘッダーを避ける）。

## 承認の方針（D-162・H8 (a)）

Mode B で承認待ち（`approval_requested`）が来たら、既定で却下する。承認するのは `eval.toml` の
`[mode_b] approve_tools`（いまは空。D-10 の試運転のツールが入ったら足す）に載ったツールだけ。ハーネスはテスト用の
利用者（dev seed の alice など）で動き、製品の認可は変えない。承認の要求とその決定はすべて記録する（S9）。

## コミットの範囲（H7 (a)）

| 置き場 | コミット |
| --- | --- |
| `results/<exp>.json`・`results/errors-<exp>.jsonl`・`results/summary.md` | する |
| `data/tasks/`・`data/versions/`・`data/captures/` | する（製品の文面はコードと同じ文字列・IR は架空のデータ） |
| `data/cache/`（会話の全文・要求の本文・試行の記録） | しない（.gitignore） |
| `results/spend.jsonl`（評価の台帳）・`results/*.tmp` | しない（.gitignore） |

課題・fixture・レポートには、実在の組織・個人の名前を書かない（public リポジトリ）。

## テスト

- `tests/conftest.py` は、置き場（台帳・キャッシュ・`ABEVAL_HOME`）をテストごとの一時の場所に向け、socket の
  接続を見張る。手元の中継（`127.0.0.1:8787`）とループバック以外への接続は例外になる（実 LLM を呼ばない）。
- 印: `server`（評価の版のサーバーが要る）・`lint_bin`（ir_lint のバイナリが要る）・`shell`（bash と git が要る）。
- 一時の置き場は `.pytest_tmp/`（/tmp は tmpfs なので使わない）。

## 限界

- 本文のバイト数による予約は大きめなので、上限の間際では最後の数件を拒むことがある（安全側）。Anthropic が
  ツールの定義に足す system の分（数百トークン）は本文に無いが、JSON の記号のバイトがそれより大きい。
- 評価の台帳は作業ツリーごとにある。評価は 1 つの作業ツリー（このブランチ）から流す。
