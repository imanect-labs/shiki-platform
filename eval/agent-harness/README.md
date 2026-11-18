# 小型モデル向けハーネスの実 LLM 評価

agent-core のループ（`run_agent`）を、OpenRouter 経由の実モデル（Gemma 4 31B・Qwen3.8 27B・
Claude Haiku 5.5）で回し、決定的に採点できる課題で「最後までやり切れるか」を測る。
設計と結果の読み方は [docs/agent/small-model-harness.md](../../docs/agent/small-model-harness.md)。

本体は Rust の例（`crates/agent-core/examples/harness_eval/`）。製品と同じループ・剪定・圧縮を
そのまま使い、課題のツールだけをメモリ上に置く。

## 課題

| 名前 | 見るもの | 採点 |
| --- | --- | --- |
| `ledger` | 長い月次報告 12 本（計 3 万トークン超）から地域別合計などを求める。途中の報告に訂正がある。32k 窓では全部を一度に載せられない | 地域別合計 4 つ・最大地域・単月最大・障害月数の部分点 |
| `codefix` | 仮想リポジトリの失敗テスト 4 件を、読む・検索・完全一致の置換・テスト実行で直す。テストの書き換えは 0 点 | 通ったテストの割合 |
| `tickets` | 規程を読み、10 件の問い合わせを顧客情報と突き合わせて処理する。境界値・優先順位・一時障害あり | 正しく処理した件数 |
| `tickets-long` | 同じ規程で 30 件（擬似乱数・正解は規程の関数で決める）。履歴が長く圧縮が要る | 同上 |

## 窓の模擬

OpenRouter のモデルは窓が広いが、オンプレの vLLM では 32k 前後で運用する。カタログの
`context_window_tokens` を `--window`（既定 32768）にし、**実測 prompt＋出力枠が窓を超えたら、
その時点で拒否されたとみなす**（vLLM と同じ文言のエラーを返す）。ハーネスはこれを窓あふれとして
扱える（圧縮してやり直す）。やり直しも失敗すれば `overflow` として記録する。

## 実行

```sh
# 会計の記録先（テスト用 DB）。migration は起動時に当てる。
export STORAGE_TEST_DATABASE_URL=postgres://postgres:pg@localhost:5432/shiki_test
cargo build -p shiki-agent-core --example harness_eval
./target/debug/examples/harness_eval --model gemma --task all --variant improved --runs 3 \
  --out eval/agent-harness/results/v4-w32k.jsonl --ledger /tmp/ledger.json --transcripts /tmp/tx
python3 eval/agent-harness/summarize.py eval/agent-harness/results/v4-w32k.jsonl
```

- `--model`: `gemma`（google/gemma-4-31b-it）・`gemma-moe`（26b-a4b）・`qwen`（qwen/qwen3.8-27b）・`haiku`（anthropic/claude-haiku-5.5）。
- `--variant`: `baseline`（圧縮と補助をすべて切る＝現行 main 相当）・`improved`（既定）・`no-compaction`・`no-budget`・`no-discipline`。
- OpenRouter のキーはプロキシが注入する前提で、プレースホルダを送る（キーを環境変数に置かない）。
- `--ledger` に推定費用をモデル群（`open`＝gemma/qwen、`haiku`）ごとに積み、`--budget-open` / `--budget-haiku`（USD）に達したら新しい run を始めない。実額は OpenRouter の `/api/v1/key` の利用額で突き合わせる（反映が遅れるので run ごとの差分は取らない）。
- `--transcripts` に run ごとの履歴とイベント列（要約付き）を書く。失敗の分類はこれを読んで行う。

## 結果

`results/*.jsonl` は 1 行 1 run。ファイル名の接頭辞が実装の版（`baseline`・`improved`・`v2`〜`v4`）。
版ごとの違いは docs の「検証ループの記録」節を参照。
