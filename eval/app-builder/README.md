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
ピークのメモリが大きいので、ほかのビルド・E2E・スタックの起動と重ねない（台帳 §9.4）。CSV の集計の隔離ランナー
（workspace の外・bundled の DuckDB）も同じ commit でビルドするので、cold なら DuckDB の C++ の分だけ長くかかる。

```bash
# 版をビルドして退避する（~/orca/workspaces/shiki-platform/abeval-<label> に worktree を作り、終わったら消す）
eval/app-builder/scripts/build-version.sh --dry-run v0 81a9899b   # 手順だけを表示
eval/app-builder/scripts/build-version.sh v0 81a9899b
#  → ~/.local/share/shiki-eval/bin/shiki-server-v0・shiki-tabular-runner-v0 と
#    data/versions/v0.json（commit・tree_clean・binary_sha256・runner_sha256・built_at・rustc）
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
| CSV の集計の隔離ランナー | `SHIKI__TABULAR__RUNNER_PATH=~/.local/share/shiki-eval/bin/shiki-tabular-runner-<label>`（版と同じ commit。作業ツリーの HEAD のランナーを使わない） |

- 起動の前に、版の記録の `tree_clean` と、バイナリとランナーの sha256 が記録と同じことを確かめる（違えば・
  ランナーの記録が無ければ起動しない）。
- `run` は、版のサーバーの `/workflows/validate` で使えるノードの種類を確かめ（未知の種類に返る
  `ir.unknown_node_type` で見る・費用 0）、版の記録の `node_types` と結果の `versions.<label>.node_types` に残す。
  課題の `requires` の `node:*` はこれで判定する（HEAD の ir_lint のカタログで判定すると、V0 に無いノードの課題が
  対象外にならない）。
- 呼び出し側のシェルに残った `SHIKI__*`・`SHIKI_CONFIG` は渡さない。条件は手数の上限（`--max-steps` →
  `SHIKI__CHAT__MAX_STEPS`・省けば製品の値）と 1 発話の予算（`--budget-micros` →
  `SHIKI__CHAT__BUDGET_MAX_COST_USD_MICROS`・既定 $0.15）だけをここで決める。
- dev-up.sh の引数は `dev-up.sh --help` を参照（`--server-bin`・`--db-name`・`--store-name`・`--bucket`・
  `--redis-db`・`--port`・`--gateway-ports`・`--api-only`・`--run-label`）。引数を省いた起動は前と同じ
  （`tests/test_dev_up_args.py` が、引数を足す前の dev-up.sh で作った記録と比べて確かめる）。ラベルのサーバーの
  停止は、TERM で 20 秒止まらなければ KILL し、それでも空かなければ止める。
- 引数を省いた dev-up.sh（:8080 のスタック）は、起動のときに `pgrep -f ^<repo>/target/debug/shiki-server$` で
  残りのサーバーを止める（前からの動き）。版のサーバーを `--server-bin <repo>/target/debug/shiki-server` で
  動かしている間に既定のスタックを起動すると、版のサーバーも止まる。版のサーバーは退避したバイナリ
  （`~/.local/share/shiki-eval/bin/`）で動かす。

## shiki のクライアント・fixture・点検（§4.4・§4.7）

**クライアント（`abeval/shiki.py` の `ShikiClient`）**: 利用者ごとに 1 つ（Cookie の入れ物を分ける）。BFF を通さず、
API（stub のスタックは `:8080`、版のサーバーは `:10067`）を製品の公開の口で叩く。

- ログインは `deploy/compose/smoke-bff.sh` と同じ OIDC の手順。Keycloak の画面の `kc-form-login` の action
  （`&amp;` を戻す）に username と password を POST し、戻り先（dev-up の既定は `:3000/auth/callback`）の scheme・
  host・port を API の基底に置き換えて `/auth/callback` を呼ぶ（302 は追わない）。
- Keycloak は http でも `Secure` の Cookie を出す。Python の cookiejar は https でないと送り返さないので、Keycloak の
  Cookie はログインの往復の間だけ自分で持ち、authorize と同じ宛先にだけ付ける。
- 状態を変える要求（POST・PUT・PATCH・DELETE）にだけ `x-csrf-token`（Cookie `shiki_csrf` の値）を付ける。
  401 なら 1 回だけログインし直す。版の機能は `capabilities()`（OpenAPI の `node_ports`・`trigger`・
  `scheduled_at`・`/me/custom-instructions`）で判定する。チャットの道具の有無（`list_workflows`）は OpenAPI に
  載らないので、取り込んだ tools で決める。
- `stub_only=True`（stub のスタック）では、model が `default`・`decide-stub` 以外の IR と発話、model を省いた
  llm.invoke・ai.decide・agent.invoke、実モデルの名前を含む値を、送る前に止める（`assert_stub_only`）。
  手元の llm-stub.toml の anthropic は本物の中継（8787）を指しているため。保存済みのワークフローを動かす前にも、
  その IR を読んで確かめる。

**fixture（`abeval/fixtures.py`）**: 課題の `fixtures`（`folders`・`tables`（`records` つき）・`csv`）を、
フォルダー → テーブル → 行 → CSV の順に作り、作った id を順に `FixtureRecord` に残す（途中で落ちても、そこまでの
分を片付けられる）。

- `@rel:-6d` は基準の時刻（Asia/Tokyo）からの相対の日付（`YYYY-MM-DD`）。`@rel:-6dT09:00` はその日の日本時間の
  時刻、`@rel:+2h`・`@rel:-30m` は時刻（どちらも RFC 3339・+09:00）。
- `@id:<ref>` は先に作った fixture の id、`@record:<テーブルの ref>:<番号>` はその行の id。
- 片付けで消すのは、記録した id（新しい順・行はテーブルごと消える）と、試行の開始の後に作られた `abeval-` の印の
  付いたもの（ルート直下のノード・テーブル・ワークフロー）だけ。本人の全資源は消さない。フォルダーとファイルは
  ゴミ箱に入る（製品にゴミ箱を空にする口は無い）。
- 番兵（alice のルート直下のフォルダー `abeval-sentinel`）が無いサーバーでは、作るのも片付けるのも拒む。

**点検（`python -m abeval.doctor`）**

```bash
uv run python -m abeval.doctor --mode stub --api-url http://localhost:8080   # stub のスタック
uv run python -m abeval.doctor --mode eval                                    # 版のサーバー（:10067・--go の前）
uv run python -m abeval.doctor --mode eval --init-sentinel                    # 評価用のサーバーに番兵を作る
```

| 項目 | 合格 |
| --- | --- |
| `/healthz` | 200 |
| alice・bob・charlie のログインと `/me` の org | alice と bob は a-corp、charlie は b-corp |
| `/llm/models` | eval は claude-haiku-5-5 と default、stub は default がある |
| 番兵 | ある（無ければ `--init-sentinel`） |
| alice のカスタム指示 | 空（about・style が空白で tone が default）。404 は「口が無い版」で空とみなす |
| OpenAPI の機能 | 表示だけ |
| ポート（eval） | 10067。:8080 の組が動いていれば警告（重ねない） |
| llm-eval.toml（eval） | 権限 600・anthropic の base_url が abeval.proxy・api_key がダミー（値は出さない）・session_header・モデルが claude-haiku-5-5 と default だけ・anthropic のモデルが中継の許可リストの中 |
| abeval.proxy（eval） | 合言葉つきの `GET /abeval/spend` が 200 |
| 手元の中継の台帳（eval） | 累計が $95 未満（読むだけ。8787 へは繋がない。無ければ警告） |
| 評価の台帳（eval） | 残りがある |
| 評価の台帳の置き場（eval） | 前の置き場（`results/spend.jsonl`）に行が無く、手元の中継の台帳の評価の分（tag `abeval/`）が評価の台帳より大きくない |

api.anthropic.com へ 401 を確かめることはしない（鍵を持たないので要らない）。

**取り込みの前の点検**（`doctor.capture_guard`・`abeval capture`・`run` の取り込み・`--regrade-from` の前に必ず通す。
`--skip-doctor` でも飛ばさない）: 取り込みは実モデルの名前（`claude-haiku-5-5`）で発話を送るので、送り先の
サーバーの anthropic が abeval.proxy を指していないと、評価の台帳を通らずに手元の中継へ届いて課金される。
API のポートが 10067（ループバック）・llm-eval.toml の base_url が abeval.proxy・そのポートを握るプロセスの
`SHIKI_CONFIG`（`ss` と `/proc/<pid>/environ` から読む）の anthropic も abeval.proxy で鍵がダミー・`SHIKI__LLM__*` の
上書き（`SHIKI__LLM__BACKEND` を除く）が無いことを確かめる。読めなければ送らない。

**サーバーの要るテスト**（stub のスタックに対して流す。実 LLM は呼ばない）:

```bash
ABEVAL_API_URL=http://localhost:8080 uv run pytest -m server -q -s
```

ログインと org・モデル・カスタム指示・番兵（無ければ作り、終わったら消す）・fixture の作成と片付け（テーブル・
ルート直下のノード・ワークフローの一覧が前と同じに戻る）・IR の検証・stub の IR の保存と実行・stub のチャットの
1 往復・charlie（別テナント）から alice の id が無い id と同じ応答（ワークフローは 403、テーブルは 404）になることを
確かめる。名前には毎回ちがう印を付ける（ほかの確かめと DB を共有しても重ならない）。

## 課題集（`data/tasks/core.json`・§3）

中核の 20 課題（§3.2）を、それぞれ 3 つの言い方（`flow`・`app`・`auto`）で書く。形の正は `abeval/tasks.py` の
docstring（§3.1 に `clarifications`・`run_cases[].llm_dependent`・`run_cases[].stub_ok`・`trigger`・`scheduled_at`・
`sequence`・`requires` を足したもの）。名前と組織はすべて架空。

- 正解の IR（`reference.ir`・2 つで 1 組の課題は `reference.irs`）は、テーブル・フォルダー・ファイルを
  `@table:<ref>`・`@folder:<ref>`・`@file:<ref>` で書き、保存の前に fixture の id に置き換える（GAP-35 の実在の
  検査を通すため）。オフラインの検査（ir_lint）では ref ごとに決まったダミーの UUID にする。正解は llm.invoke の
  model を書かない（組織の既定で動く）。stub で流すときだけ `default` にする（`run_cases.prepare_ir`）。
- 相対の日付は使わず、日付で結果が決まる課題（先週・今日）は run_case の `scheduled_at` で予定の日時を決める
  （GAP-88・`requires` に `cap:scheduled_at`）。
- fixture のテーブルは、絞り込み・集計・並べ替えに使う列を `indexed: true` にする（索引の無い列は実行で失敗する）。
  担当者ごとの合計を見る課題は `aggregate_min_rows: 1`、APP-08 だけは既定の K（5）のままにして、件数の少ない
  地域が伏せられることを期待値に入れる（PIT-17）。状態の進め方（FSM）は fixture で作らないので、状態は選択肢の列を
  書き換える形にしている（data.transition は使わない）。
- 対象外（このマシンで動かない・評価の設定に判定用のモデルが無い）: ai.decide・RAG・agent.invoke・Office。
  APP-09（きっかけがファイルの保存）は、有効化を使わずに、手動の起動にきっかけの内容（`trigger`）を渡して流す
  （`requires: trigger:event・cap:trigger`）。
- 分類を照合する課題（APP-02・APP-05・APP-12）の run_case は `llm_dependent`。stub では照合せず、3 回中 3 回の
  確かめは実モデルで測る段（T10）で行う。
- 何も変えないのが正解の run_case（在庫が足りない・貸出中）は `expect.flow_through: false`（入力が届かない検査を
  しない）。中身の無い IR はこの run_case を通り得るので、null の確かめは課題の単位（S4 は全 run_case が通ること）。

## 採点（`abeval/grade/`・§5）

| 置き場 | 中身 |
| --- | --- |
| `grade/static.py` | 1 層目 ir_lint（`errors`・`run_blockers`・`STRICT_WARNINGS` は S2s、ほかの警告は S2w、`ignored_by_env` は数えない）。2 層目は課題の検査（入力の参照・`flow_through`・出力の path〔カタログの `output_fields` と `present_if`〕）と S5（トリガ・ノードの種類・診断用） |
| `grade/dynamic.py` | dead edge（`node_ports` に無い出口・出口が 1 つのノードの通らなかった線）・差し込みの残り（`{name}`・`{{…}}`）・入力が届かない（stub の run だけ。実モデルの出力は入力の全文を含まない）・想定外の状態（stub で流した `llm_dependent` でない run_case の状態が期待と違う）・想定外の skipped（成功した run で、分岐でも error の出口でもない線の先が飛ばされた。元の 1 つだけを数える）。`llm_dependent` を stub で流したときは最初の LLM のノードまでに限る |
| `grade/checks.py` | 照合子（equals・contains・regex・in・absent・rows・files・no_new_rows）。NFKC と数字の正規化。`$fixture` で fixture から期待値を計算する。実行の日に決まる値（貸出日・ファイル名の日付）は `{"$run_date": {}}`・name_regex の `@run_date` で、run を始めた日（日本時間・`$from run /date` と同じ）と照合する（形の正規表現にしない） |
| `grade/mutate.py` | 採点器の自己テストの 8 通りの壊し方と、期待する検査。中身の無い IR（null） |
| `gen_checks.py` | 課題の検査 G1〜G9 のうちオフラインのもの（G2・G4・G5・G7・G8・G9）。G8 の拒否リストは `~/.config/shiki-dev/abeval-denylist.txt`（リポジトリの外・1 行 1 語）。無ければ理由を出して飛ばす。理由には場所と語の長さだけを書く |
| `run_cases.py` | run_case をサーバーで流して記録を集める（fixture の行の入れ直し・起動の列・成功した step の出力・行・増えたファイル・再実行）。正解の結合テストと、後の段の Mode A・B が使う |
| `grading.py` | 試行の採点の部品。基盤の失敗（保存できない・時間切れ・5xx など）で流せなかった run_case が 1 つでもあれば、S2d・S3・S4 を None にして試行を errors に回す（部分だけで採点しない）。見出しは S2s・S2d（Mode B は S4 も）が決まったときだけ真で、決まらなければ None（率の分母に入れない） |

採点器の版は `grade.GRADER_VERSION` と ir_lint のバイナリの sha（`static.grader_version()`）で残す。

## 取り込み・Mode A・Mode B・計画・レポート（§4.5〜§4.7・§6・§8）

```bash
uv run python -m abeval plan --mode a --label v0 --exp e1a                 # 計画と見積もりだけ（費用 0）
uv run python -m abeval run  --mode a --label v0 --exp e1a                 # 同じ（--go が無ければ計画だけ）
uv run python -m abeval run  --mode a --label v0 --exp e1a --go            # 点検 → 登録 → 取り込み → 試行 → 結果
uv run python -m abeval run  --mode a --label v1 --exp e1a-v1 --regrade-from v0   # V1: 生成せず採点し直す（$0）
uv run python -m abeval run  --mode b --label vn --exp e1b-vn --trials 3 --go
uv run python -m abeval run  --mode b --label stub --exp stub-b --stub --api-url http://localhost:8080  # stub の配管だけ
uv run python -m abeval capture --label v0 --exp e1a                       # 取り込みだけ（$0）
uv run python -m abeval grade  --exp e1a                                   # 結果の IR を ir_lint で採点し直す
uv run python -m abeval report                                             # results/summary.md
uv run python -m abeval budget                                             # 評価の累計・実行ごと・総額の残り
```

共通の引数: `--tasks a,b`（既定は全部）・`--phrasings flow,app,auto`・`--trials 3`・`--effort low|medium|high|auto`
（重ねて指定できる）・`--max-steps N`（版のサーバーを `serve --max-steps N` で起動しておく・条件の印）。
版に無い機能の要る課題（`requires`）は「対象外」として外し、結果の `out_of_scope` に残す。

| 置き場 | 中身 |
| --- | --- |
| `abeval/capture.py` | 取り込み。abeval.proxy に `capture_only` の実行を登録し、新しいスレッドに短い発話（`claude-haiku-5-5`・effort あり）を送って、保存された要求のうち emit_workflow を含む最初のものから system・tools・max_tokens を `data/captures/<label>/<exp>/` に書く。「今日の日付」の一文（製品の `opts.rs` の書式）は `[capture] fixed_date` に置き換え、tools の説明文の fixture のテーブルの id は印（`@table:<ref>`）に戻す。指紋（llm-eval.toml の sha・カスタム指示・workflow と decision のモデル・fixture の名前・OpenAPI の機能・tools と system の sha）を meta.json に残し、前の取り込みと違う項目を `changed_from_previous` に出す。llm-eval.toml の鍵の形の項目がダミーでなければ止める |
| `abeval/anthropic_client.py` | httpx で Messages を stream で送る（SDK なし）。temperature・top_p・top_k と prefill は送る前に止める。再試行は 429・5xx（529）・接続の誤り・流れの途中の overloaded だけ（400・403 は再試行しない）。全幅のジッタ・`retry-after` を守る・回数を記録する |
| `abeval/mode_a.py` | Mode A（単発）。課題ごとに fixture を作り（課題の間で名前が重なるため）、参考情報（入力の項目の key・呼び名・型、テーブルの表示名・id・列の key と型、フォルダー・ファイルの id・`trial.reference_info`）を添えて生成する。tools は取り込んだ emit_workflow だけ（defer_loading を外す）・thinking adaptive・`output_config.effort`・tool_choice auto。結果は refusal → truncated → emit → asked → no_emit の順に分ける。S1 はその版の validate、S2s・S2w・S5 は ir_lint、S2d・S3 は stub の配管（流せなかったら errors の `stub_plumbing`） |
| `abeval/mode_b.py`・`abeval/approvals.py` | Mode B（製品のチャット・並列度 1）。SSE は承認の要求を拾うためにだけ使い、完了は `GET messages` の `active_run_id` を `[mode_b] poll_secs` ごとに読み直す。依頼文には Mode A と同じ参考情報を添える。壁時計の上限（`wall_clock_secs`・6 分）で取り消して timeout。保存の前の聞き返し（「？」で終わる・課題の `ask_terms`）には `clarifications` を 1 回だけ（S8 の `asked` は「保存の前に聞き返した」。保存した後の「有効化しますか？」は数えない）。会話が誤りで終わった・abeval.proxy が拒んだ・上流に繋がらない・run_case を流せなかった試行は採点もキャッシュもせず errors に入れる（kind は llm_error・upstream・budget・run_case・stub_plumbing。budget は残りを打ち切る）。各試行の前に、実行と評価の残りが 1 試行の最悪より少なければ打ち切る。emit_workflow の入力の IR は validate にかけ直して検証エラーを数える（結果の文を読まない）。保存されたワークフローそのもので run_case を流し（評価の版は実モデル・費用は active で試行に割り当てる）、stub の配管も流す。採点の後の hook（`ModeBContext.hooks`）はミニアプリ（T12）の差し込み口 |
| `abeval/plan.py` | 試行の列（Mode A は言い方 × 試行、Mode B は言い方を試行で回す）と見積もり（`[estimates]`・Mode A は取り込みから組んだ予約の額と大きい方）。最悪が `run_cap_usd` を超えれば拒み、課題を分けた組を示す。評価の台帳＋最悪が `eval_cap_usd`、手元の中継の台帳＋最悪が `global_cap_usd` を超えても拒む。`--go` のときだけ abeval.proxy に登録する（上限は min(最悪 × 1.2, $10)） |
| `abeval/budget.py` | 評価の台帳の累計・実験ごと・実行ごと（usage が無く予約で数えた行の数）・手元の中継の累計・残り（読むだけ） |
| `abeval/metrics.py` | S1〜S9・課題ごと・pass@k の不偏推定・課題を単位にした対応のあるブートストラップ（10,000 回・種は固定） |
| `abeval/report.py` | `results/<exp>.json`（同じ実験は試行をキーで置き換えて足す）・`errors-<exp>.jsonl`・`summary.md`（6 節）。書く前に、会話の全文と本文の項目・鍵の形の値・拒否リスト（G8）の語を検査し、見つかれば何も書かない |

- 生成と会話の全文（応答のブロック・メッセージ一覧・承認の入力）はキャッシュ（`data/cache/trials/`）にだけ置く。
  結果には保存された IR（fixture の id は印に戻したもの）と採点だけを入れる。
- キャッシュのキーは、依頼文（参考情報の id は印のまま）・fixture・試行の番号・条件・llm-eval.toml の sha・
  取り込みの sha・バイナリの sha。Mode B は採点した結果を置くので、採点器の版（GRADER_VERSION と ir_lint の
  sha）と課題の期待（expect・roles・run_cases）の sha も入れる（採点器か課題を直したら流し直す。Mode A は生成
  だけを置き、採点は毎回やり直す）。同じ条件で流し直すと生成も会話もしない（台帳に費用の行が増えない。取り込みの
  $0 の行だけが足される）。
- 費用の割り当て: Mode A は会話 ID のヘッダーに試行の id を載せる。Mode B は会話 ID（thread_id）を試行に、
  run_case の llm.invoke（会話 ID が無い）は active を試行にする。突き合わせは `/org-admin/ai/usage` の前後の差
  （1% 以内・SQL は使わない）。
- 失敗の分け方: モデルの結果（保存できない・壊れている・聞き返し・拒否・承認の要求）は結果に、基盤の失敗
  （再試行切れ・timeout・fixture の作成・予算切れ・会話の誤り・流せなかった run_case と stub の配管・片付けの残り）
  は `errors-<exp>.jsonl` に入れる（採点しない）。費用の上限で拒まれたら、その実行の残りを打ち切る。送る前に
  拒んだ要求と上流に繋がらない要求は台帳に載らないので、abeval.proxy が実行ごとに数える（`GET /abeval/spend` の
  `rejected`・`unreachable`）。

## E0（試走）と E1 を流す手順（親が行う）

上限: 評価の累計 $35（abeval.proxy が評価の台帳で送る前に強制）・1 回の実行 $10（plan が拒み、abeval.proxy が
登録の額で止める）・総額 $95（手元の中継が強制・ここでは plan と budget が表示と判定に使う）。実 LLM は
Claude Haiku 5.5 だけ（abeval.proxy の許可リスト）。

1. 版をビルドする（別の worktree・1 版 7 分程度・ほかのビルドや E2E やスタックと重ねない）。

   ```bash
   eval/app-builder/scripts/build-version.sh v0 81a9899b       # 修正前（D-54 の 1）
   eval/app-builder/scripts/build-version.sh v1 71e9b79a       # Mode B だけ測る・Mode A は採点し直し
   eval/app-builder/scripts/build-version.sh vn <BUG-3 と GAP-29 が着地したきれいな commit>
   ```

2. 設定と中継を用意する。

   ```bash
   install -m 600 eval/app-builder/config/llm-eval.example.toml ~/.config/shiki-dev/llm-eval.toml
   ss -ltn 'sport = :8787'                       # 手元の中継が動いているか（繋がない）
   cd eval/app-builder && uv run python -m abeval budget   # 評価と総額の残り（台帳を読むだけ）
   uv run python -m abeval.proxy                  # 別の端末で（127.0.0.1:18200 → 127.0.0.1:8787）
   ```

3. 版のサーバーを起動して点検する（キャンペーンの :8080 の組と重ねない）。

   ```bash
   uv run python -m abeval serve --label vn
   uv run python -m abeval.doctor --mode eval --init-sentinel    # 初めての DB だけ
   uv run python -m abeval.doctor --mode eval                    # すべて合格を確かめる
   ```

4. E0（試走・A 12・B 12・最悪 $0.7 前後）。計画を見てから `--go` を付ける。

   ```bash
   T=basic-summary-file,app01-claim-submit,app03-weekly-summary,app11-checkout
   uv run python -m abeval run --mode a --label vn --exp e0a --tasks $T --trials 1
   uv run python -m abeval run --mode a --label vn --exp e0a --tasks $T --trials 1 --go
   uv run python -m abeval run --mode b --label vn --exp e0b --tasks $T --trials 3 --go
   uv run python -m abeval budget
   ```

   results/e0a.json・e0b.json の `aggregates[].cost_usd` を試行の数で割った額で `eval.toml` の `[estimates]` を
   置き換える（推測の見積もりを実測にする）。abeval.proxy の台帳と `/org-admin/ai/usage` の突き合わせ
   （Mode B の `cost.reconcile.ok`）も見る。

5. E1（版の軸）。1 回 $10 を超えないよう、実行を分ける（plan が超える実行を拒み、分け方を示す）。

   ```bash
   uv run python -m abeval serve --label v0 && uv run python -m abeval.doctor --mode eval
   uv run python -m abeval run --mode a --label v0 --exp e1a-v0 --go       # A 180（最悪 $2.2 前後）
   uv run python -m abeval run --mode b --label v0 --exp e1b-v0 --go       # B 60（最悪 $3 前後）
   uv run python -m abeval serve --label v0 --down
   uv run python -m abeval serve --label v1 && uv run python -m abeval.doctor --mode eval
   uv run python -m abeval run --mode a --label v1 --exp e1a-v1 --regrade-from v0   # 生成しない・$0
   uv run python -m abeval run --mode b --label v1 --exp e1b-v1 --go
   uv run python -m abeval serve --label v1 --down
   uv run python -m abeval serve --label vn && uv run python -m abeval.doctor --mode eval
   uv run python -m abeval run --mode a --label vn --exp e1a-vn --go
   uv run python -m abeval run --mode b --label vn --exp e1b-vn --go
   uv run python -m abeval report && uv run python -m abeval budget
   ```

   - `--regrade-from v0` は、V1 の取り込みの sha が V0 と同じとき（説明文が同じとき）だけ採点し直す。違えば止める。
   - 取り込みの指紋が前と違えば（設定・モデル・カスタム指示・fixture・機能・説明文）、実行の始めに表示し、
     結果の `versions.<label>.capture_changed` に残る。
   - 使った額は `python -m abeval budget` で読み、台帳 §10 に実験名と合計を記録する（human への提案）。
   - コミットするのは results/<exp>.json・errors-<exp>.jsonl・summary.md・data/captures/・data/versions/ だけ。

## 上限と台帳

| 項目 | 既定 | 強制する場所 |
| --- | --- | --- |
| モデルの許可リスト | `claude-haiku-5-5` | abeval.proxy（送る前） |
| 1 回の実行（`run_cap_usd`） | $10（登録した額と小さい方） | abeval.proxy（送る前・実際の消費と予約で） |
| 評価の累計（`eval_cap_usd`） | $35（月で区切らない） | abeval.proxy（送る前） |
| 外側の歯止め（`global_cap_usd`） | $95（2026-10-25 からの累計） | 手元の中継（ここでは表示だけ） |

- 評価の台帳は `~/.local/share/shiki-eval/ledger.jsonl`（作業ツリーの外・権限 600・1 行 = 1 回の要求・
  `cost_micros` はマイクロ USD の整数）。上限の正本なので**消さない**。壊れた行があれば読み込みで止まる。
  作業ツリーを作り直しても、別の checkout から流しても同じ台帳で数える。
- 消えた・別の台帳になったときの守り（`budget.ledger_guard`）: 手元の中継の台帳のうち tag が `abeval/` で始まる
  行の累計が評価の台帳より大きければ、abeval.proxy は起動を拒み、doctor は NG、plan は大きい方で数える。
- 前の既定（作業ツリーの `results/spend.jsonl`）に行が残っていると、abeval.proxy は起動を拒む。移し方:

  ```bash
  mkdir -p -m 700 ~/.local/share/shiki-eval
  cat eval/app-builder/results/spend.jsonl >> ~/.local/share/shiki-eval/ledger.jsonl
  chmod 600 ~/.local/share/shiki-eval/ledger.jsonl
  rm eval/app-builder/results/spend.jsonl   # 足したことを確かめてから
  ```
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
| 評価の台帳（`~/.local/share/shiki-eval/ledger.jsonl`・作業ツリーの外）・`results/spend.jsonl`（前の置き場）・`results/*.tmp` | しない（.gitignore） |

課題・fixture・レポートには、実在の組織・個人の名前を書かない（public リポジトリ）。

## テスト

- `tests/conftest.py` は、置き場（台帳・キャッシュ・`ABEVAL_HOME`・結果 `ABEVAL_RESULTS_DIR`・取り込み
  `ABEVAL_CAPTURES_DIR`）をテストごとの一時の場所に向け、socket の接続を見張る。手元の中継（`127.0.0.1:8787`）とループバック以外への接続は例外になる（実 LLM を呼ばない）。
- 印: `server`（stub のスタックか版のサーバーが要る・`ABEVAL_API_URL`）・`lint_bin`（ir_lint のバイナリが要る）・`shell`（bash と git が要る）。
- `shell` のテストは docker・cargo・curl などを shim（`tests/shellkit.py`）にして流す（本物は動かさない）。
  `test_dev_up_args.py` は、引数を省いた dev-up.sh（と `--status`・`--down`）が、引数を足す前の版（0a0567a1）で
  作った記録（`tests/fixtures/dev_up/`）と同じ run-server.sh を作り、同じ shim を同じ順に呼ぶことを確かめる
  （既定の経路を意図して変えたときだけ `ABEVAL_REGEN_DEV_UP=1` で作り直す）。`test_build_version.py` は一時の git リポジトリと偽の cargo で流す。
- 一時の置き場は `.pytest_tmp/`（/tmp は tmpfs なので使わない）。
- 採点器のオフラインのテストは、記録（`tests/fixtures/ir_lint/`・`tests/fixtures/runs/`）を読む。課題を変えたら
  記録し直す（記録の入力の sha が違うとテストが止まる）。記録と実物の一致は `pytest -m lint_bin` が見る。

  ```bash
  uv run python scripts/record_fixtures.py --lint                                   # ir_lint の出力
  uv run python scripts/record_fixtures.py --runs --api-url http://localhost:8080   # stub のスタックで run を記録
  ABEVAL_API_URL=http://localhost:8080 uv run pytest -m server tests/test_reference_it.py -q   # 正解と null
  ```

- Mode A・取り込み・計画のオフラインのテストは、偽の shiki（`tests/fake_chat.py`・製品の要求のまねを中継へ送る）・
  テストの中で動かす abeval.proxy（`tests/inproc.py`・一時のポート）・偽の上流（Anthropic の SSE・
  `tests/fake_anthropic.py`）で、CLI（`run --go`・キャッシュ・`--regrade-from`）まで通す。Mode B は台本どおりに
  動く偽のチャット（承認の要求・終わらない run・空の SSE・聞き返し）と偽の時計で流す。
- Mode B の記録の読み取りは、stub のスタックで記録した会話（`tests/fixtures/transcripts/`・stub の応答だけ）を
  読む。作り直すときは次のとおり。

  ```bash
  ABEVAL_RECORD_TRANSCRIPTS=tests/fixtures/transcripts ABEVAL_API_URL=http://localhost:8080 \
    uv run pytest -m server tests/test_mode_b_it.py -q
  ```

- サーバーの要る Mode A・Mode B の確かめ（`test_mode_a_it.py`・`test_mode_b_it.py`）は stub のスタックで流す
  （発話の model は `default`・依頼文を `calltool:` に差し替えた試験用の課題）。取り込みの確かめ
  （`test_capture_it.py`）は、API のポートを握るサーバーの `SHIKI_CONFIG` の anthropic が `127.0.0.1:18200`
  （abeval.proxy）を指し、鍵がダミーのときだけ流す（テストの中で capture_only の abeval.proxy を 18200 で動かす・
  上流は繋がらない宛先）。それ以外のスタックでは理由を出して飛ばす。
- 採点器の自己テスト（`tests/test_mutate.py`）は、8 通りの壊し方が期待した検査で捕まることと、検査を何もしない
  ように差し替えるとそのテストが落ちることを確かめる。`from_port: next` は HEAD の検証では保存できないので、
  正解の記録から V0 の動き（その線の先が動かない）を組んで dead edge を見る。

## 限界

- 本文のバイト数による予約は大きめなので、上限の間際では最後の数件を拒むことがある（安全側）。Anthropic が
  ツールの定義に足す system の分（数百トークン）は本文に無いが、JSON の記号のバイトがそれより大きい。
- 評価の台帳はこの機械の 1 つ（作業ツリーの外）。別の機械から流すと別の台帳になる（手元の中継の台帳との
  突き合わせは、同じ機械の中継だけを見る）。
- 取り込みの capture_only の要求も、評価の台帳に $0 の行として残る（キャッシュで生成を飛ばした実行でも、取り込みの
  行だけは増える）。
- Mode B の聞き返しの判定は単純な規則（「？」で終わる・課題の `ask_terms`）なので、実モデルでは取りこぼしがある。
  S8 の `unknown`（保存も聞き返しもしないで終えた）に残す。
- 製品のチャットには、Drive のフォルダーやファイルの id を調べる道具が無い（fs_* はスレッドの作業フォルダーの中
  だけ）。そのため Mode B にも Mode A と同じ参考情報（id と入力の key）を添えて測る。製品の経路だけで id を知れる
  かは、この評価では測らない。
- 製品の `/org-admin/ai/usage` は日の単位なので、突き合わせは試行の前後の差で見る（並列度 1・同じ日のうちの差）。
  ほかの利用が同じサーバーにあると合わない。
- 差し込みの残りと入力が届かないの検査は、stub が prompt を「回答: …」で返すことに頼る。出力の形（output_schema）
  のある llm.invoke では stub が形に合う最小の値を返すので、その prompt の差し込みの残りは動的には見えない
  （静的な `ir.template_escaped_var` で捕まえる）。
