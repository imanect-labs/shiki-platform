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
uv run python -m abeval.proxy         # 記録・上限つきの中継（127.0.0.1:18200 → 手元の中継）
```

ir_lint の使い方（標準入力の JSONL → 標準出力の JSONL・誤りがあっても終了コード 0）:

```bash
echo '{"id":"t1","ir":{...}}' | target/debug/examples/ir_lint
target/debug/examples/ir_lint --catalog   # ノードのカタログ（出力の形）と警告のコードの表
```

## 中継（`abeval.proxy`）

製品（評価の版のサーバー）と Mode A の生成は、Anthropic への要求をすべて `127.0.0.1:18200` に送る。中継は
上流（既定は手元の中継 `http://127.0.0.1:8787`。`--upstream` か `eval.toml` で変える）へ転送し、台帳に記録して
上限で止める。ループバックにだけ bind する（0.0.0.0 などは起動の前に拒む）。

- 鍵は扱わない。上流へ送るヘッダーは `anthropic-version`・`anthropic-beta`・`content-type`・`accept` だけで、
  `x-api-key` はダミー（`abeval-dummy`）に置き換える。クライアントの `x-api-key`・`authorization`・会話 ID の
  ヘッダー（`x-abeval-session`）は送らない。本物の鍵は手元の中継が付ける。
- 送る前に拒む（上流を呼ばずに Anthropic 形式の 400 `invalid_request_error`・「abeval: …」）:
  許可リストに無いモデル・`temperature`／`top_p`／`top_k` を含む本文・登録した実行（run）に割り当たらない要求
  （会話 ID で引き、無ければ active の実行）・評価の累計の上限・実行の上限。
- 上限の判定は「台帳の累計＋処理中の予約＋この要求の最悪」。最悪は、入力を本文のバイト数、出力を
  `max_tokens` で見積もる（本文が 100K バイトを超えたら高い段の単価）。並列に送っても予約で数えるので越えない。
- 応答は状態コードを含めてそのまま返し（429・403・5xx も）、SSE はチャンクごとに中身を変えずに流す（バッファ
  しない）。応答は HTTP/1.0 の close 区切り（手元の中継と同じ）。usage は `message_start` と `message_delta` の
  フィールドごとの大きい方（製品の `providers/usage.rs` と同じ）。
- 終わったら実際の費用を台帳に 1 行書いて予約を戻す。最後の usage（`message_delta`）が取れなければ（途中で
  切れた・呼び出し側が切った）予約の額で数え、`usage_missing` の印を付ける。誤りの応答（4xx・5xx）を読み切った
  ものは 0。上流に繋がらなければ 502 で、台帳に書かない。
- `POST /v1/messages/count_tokens` は台帳に載せない（許可モデルと実行の割り当ては見る）。
- 制御の口（ヘッダー `x-abeval-control` に合言葉が要る。無ければ 403。合言葉は起動時に
  `~/.local/share/shiki-eval/proxy.token`（権限 600）に書く）:

  | 口 | 中身 |
  | --- | --- |
  | `POST /abeval/runs` | `{run_id, exp, cap_usd, capture_only?}`。実行の上限は `min(cap_usd, run_cap_usd)` |
  | `DELETE /abeval/runs/{id}` | 登録を消す（その実行の会話 ID と active も外す） |
  | `POST /abeval/sessions` | `{session, trial, run_id}`。会話 ID（チャットの thread_id）を試行に割り当てる |
  | `PUT /abeval/active` | `{run_id, trial}`（`run_id: null` で外す）。会話 ID の無い要求（ワークフローの llm.invoke）の行き先 |
  | `GET /abeval/spend` | 評価の累計・予約・実行ごとの累計と上限 |

- `capture_only` の実行では、要求の本文を `data/cache/requests/<run>/` に保存し、上流へ送らずに最小の SSE
  （テキスト「abeval capture」で `end_turn`）を返す（説明文の取り込みを $0 で行う）。`--capture` を付けると、
  転送した要求の本文も同じ場所に保存する（鍵はヘッダーにあり、本文には入らない）。
- 手元の中継の台帳で用途を分けられるよう、`x-campaign-tag: abeval/<exp>/<run>/<trial>` を付ける。

## 版（ビルドと起動・§4.1・§4.2）

版は「きれいな commit から別の worktree でビルドしたバイナリ」（H1 (a)・D-54）。ビルドは 1 版 7 分程度（sccache）・
ピークのメモリが大きいので、ほかのビルド・E2E・スタックの起動と重ねない（台帳 §9.4）。

```bash
# 版をビルドして退避する（~/orca/workspaces/shiki-platform/abeval-<label> に worktree を作り、終わったら消す）
eval/app-builder/scripts/build-version.sh --dry-run v0 81a9899b   # 手順だけを表示
eval/app-builder/scripts/build-version.sh v0 81a9899b
#  → ~/.local/share/shiki-eval/bin/shiki-server-v0 と data/versions/v0.json（commit・tree_clean・binary_sha256・built_at・rustc）
```

- label は英小文字で始まり英小文字か数字で終わる 21 文字まで（DB・store・バケットの名前に使う）。
- worktree・バイナリ・版の記録が既にあれば拒む。`git status --porcelain` が空でない（汚れた）worktree と、
  ビルドが作業ツリーを書き換えた場合は止める（素性を確かめられない版は測らない）。worktree は失敗しても消す。

版のサーバーは、dev-up.sh の版のサーバー向けの引数（H3 (a)）で、API だけを専用の DB・store・バケット・Redis の
番号・ポートで起動する（Web とゲートウェイの外部の起動はしない）。

```bash
# LLM の設定（鍵は入れない。base_url は abeval.proxy・api_key はダミー・session_header は x-abeval-session）
install -m 600 eval/app-builder/config/llm-eval.example.toml ~/.config/shiki-dev/llm-eval.toml

cd eval/app-builder
uv run python -m abeval serve --label v0 --dry-run      # dev-up.sh の呼び出しを表示するだけ
uv run python -m abeval serve --label v0 [--max-steps 12] [--budget-micros 150000]
uv run python -m abeval serve --label v0 --down
```

| 分けるもの | 値 |
| --- | --- |
| DB | `shiki_eval_<label の - を _ に>`（無ければ dev-up が作る。migration と dev seed はサーバーが流す） |
| OpenFGA の store・S3 のバケット | `shiki-eval-<label>`（サーバーが作る） |
| Redis の DB 番号 | label から決まる 4〜15 |
| API・ゲートウェイ | `:10067`（Keycloak が許すリダイレクト先）・`18190`/`18191` |
| PID ファイル・ログ | dev-up の `--run-label abeval-<label>` の置き場（同じバイナリで動く別のサーバーを pgrep で止めない） |
| LLM の設定 | `SHIKI_CONFIG=~/.config/shiki-dev/llm-eval.toml`（権限 600 でなければ起動しない） |

- 起動の前に、版の記録の `tree_clean` と、バイナリの sha256 が記録と同じことを確かめる（違えば起動しない）。
- 呼び出し側のシェルに残った `SHIKI__*`・`SHIKI_CONFIG` は渡さない。条件は手数の上限（`--max-steps` →
  `SHIKI__CHAT__MAX_STEPS`・省けば製品の値）と 1 発話の予算（`--budget-micros` →
  `SHIKI__CHAT__BUDGET_MAX_COST_USD_MICROS`・既定 $0.15）だけをここで決める。
- dev-up.sh の引数は `dev-up.sh --help` を参照（`--server-bin`・`--db-name`・`--store-name`・`--bucket`・
  `--redis-db`・`--port`・`--gateway-ports`・`--api-only`・`--run-label`）。引数を省いた起動は前と同じ
  （`tests/test_dev_up_args.py` が HEAD の dev-up.sh と比べて確かめる）。
- 引数を省いた dev-up.sh（:8080 のスタック）は、起動のときに `pgrep -f ^<repo>/target/debug/shiki-server$` で
  残りのサーバーを止める（前からの動き）。版のサーバーを `--server-bin <repo>/target/debug/shiki-server` で
  動かしている間に既定のスタックを起動すると、版のサーバーも止まる。版のサーバーは退避したバイナリ
  （`~/.local/share/shiki-eval/bin/`）で動かす。

## 上限と台帳

| 項目 | 既定 | 強制する場所 |
| --- | --- | --- |
| モデルの許可リスト | `claude-haiku-5-5` | abeval.proxy（送る前） |
| 1 回の実行（`run_cap_usd`） | $10（登録した額と小さい方） | abeval.proxy（送る前・実際の消費と予約で） |
| 評価の累計（`eval_cap_usd`） | $35（月で区切らない） | abeval.proxy（送る前） |
| 外側の歯止め（`global_cap_usd`） | $95（2026-10-25 からの累計） | 手元の中継（ここでは表示だけ） |

- 評価の台帳は `results/spend.jsonl`（1 行 = 1 回の要求・`cost_micros` はマイクロ USD の整数）。上限の正本なので
  **消さない**（.gitignore 済み。`git clean -x` で消えるので注意）。壊れた行があれば読み込みで止まる。
- 手元の中継の台帳（`~/.local/state/shiki-campaign/anthropic-spend.jsonl`）は読むだけ（8787 へは繋がない）。
- 単価は `eval.toml` の `[pricing]`（台帳 §6.1）。段の境は「入力＋キャッシュの読み＋キャッシュの書き」が 100,000
  トークンを超えたら高い段（手元の中継の `cost_of` と同じ）。1 回分は切り上げの整数で数える。

## 要求の形

- `temperature`・`top_p`・`top_k` は送らない（Haiku 5.5 は 400 を返す。製品の Anthropic アダプタも送らない）。
  中継も、これらを含む本文を拒む（ハーネスの誤りを止める）。assistant の prefill もしない。
- Mode A は httpx で Anthropic の形式を直接組む（SDK は入れない。自動の再試行と勝手に付くヘッダーを避ける）。

## 承認の方針（D-162・H8 (a)）

Mode B で承認待ち（`approval_requested`）が来たら、既定で却下する。承認するのは `eval.toml` の
`[mode_b] approve_tools`（いまは空。D-10 の試運転のツールが入ったら足す）に載ったツールだけ。ハーネスはテスト用の
利用者（dev seed の alice など）で動き、製品の認可は変えない。承認の要求とその決定はすべて記録する（S9）。

## コミットの範囲（H7 (a)）

| 置き場 | コミット |
| --- | --- |
| `results/<exp>.json`・`results/errors-<exp>.jsonl`・`results/summary.md` | する |
| `data/tasks/`・`data/versions/`・`data/captures/` | する（製品の文面はコードと同じ文字列・IR は架空のデータ・版の記録は commit と sha だけ） |
| `data/cache/`（会話の全文・要求の本文・試行の記録） | しない（.gitignore） |
| `results/spend.jsonl`（評価の台帳）・`results/*.tmp` | しない（.gitignore） |

課題・fixture・レポートには、実在の組織・個人の名前を書かない（public リポジトリ）。

## テスト

- `tests/conftest.py` は、置き場（台帳・キャッシュ・`ABEVAL_HOME`）をテストごとの一時の場所に向け、socket の
  接続を見張る。手元の中継（`127.0.0.1:8787`）とループバック以外への接続は例外になる（実 LLM を呼ばない）。
- 印: `server`（評価の版のサーバーが要る）・`lint_bin`（ir_lint のバイナリが要る）・`shell`（bash と git が要る）。
- `shell` のテストは docker・cargo・curl などを shim（`tests/shellkit.py`）にして流す（本物は動かさない）。
  `test_dev_up_args.py` は、引数を省いた dev-up.sh が HEAD の版と同じ run-server.sh を作り、同じ shim を同じ順に
  呼ぶことを確かめる。`test_build_version.py` は一時の git リポジトリと偽の cargo で流す。
- 一時の置き場は `.pytest_tmp/`（/tmp は tmpfs なので使わない）。

## 限界

- 本文のバイト数による予約は大きめなので、上限の間際では最後の数件を拒むことがある（安全側）。Anthropic が
  ツールの定義に足す system の分（数百トークン）は本文に無いが、JSON の記号のバイトがそれより大きい。
- 評価の台帳は作業ツリーごとにある。評価は 1 つの作業ツリー（このブランチ）から流す。
