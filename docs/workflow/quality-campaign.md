# ワークフロー／アプリ品質向上キャンペーン（作業台帳）

> ワークフローとミニアプリをプロダクション品質に引き上げるための作業を、この 1 ファイルにまとめる。
> 設計の正本は [miniapp-platform.md](../miniapp-platform.md)・[README](./README.md)・[ir.md](./ir.md)・[engine.md](./engine.md)・[data-platform.md](../data-platform.md) にある。
> 本書は「何を試し、何が壊れていて、何を直したか」の記録である。
> 設計と食い違う点を見つけたら §8 に書いて human に提案する（正本を勝手に直さない）。
>
> 調査の基準: origin/main `81a9899b`（2026-10-25）。行番号はこの版のもの。

## 0. 進め方

### 0.1 ループ

1. ビジネスシーンを想定した実践的なアプリ（§7 の APP-xx）を shiki 上で作る
2. 利用者としてそれを使う（Playwright で画面を操作する）
3. 動作を確かめる（裏取りはテーブルの行・ドライブのファイル・run の step 出力で行い、画面の文言だけで判定しない）
4. 問題があれば BUG／GAP として §3〜§5 に起票し、直す
5. APP の成功判定をすべて満たすまで 1〜4 を繰り返す

### 0.2 決まりごと

- 1 つの直しは 1 つの PR にまとめ、PR 番号を §3〜§5 の表に書く。GitHub Issue を作ったら番号を表に書く。
- 結合テストは環境変数が無いと黙ってスキップされる。「緑だった」ではなく、意図したテストが実際に走ったことを出力で確かめる。
- 実 LLM の検証では、シナリオのデータを架空のものに限る。客先名や官公庁を連想させる名前は使わない（public リポジトリ）。
- 手元の shiki-server と結合テストを同じ DB で同時に動かさない（サーバのワーカーがテストのジョブを横取りする）。

## 1. 前提（human と合意）

- **ミニアプリの既定の保存先はデータテーブル（Phase 15 の `crates/data`）。**
  シナリオのアプリはまずデータテーブルで組み、CSV ノード（`csv.*`）はドライブ上のファイルを処理するときだけ使う。
- ワークフロー側のデータテーブルノードはエンジンとエディタの両方にある。
  - エンジン: `crates/workflow-engine/src/vocab/node_type.rs` の `data.query` / `data.record.{create,update,delete}` / `data.transition` / `data.bulk_upsert`
  - エディタ: `config-panel/forms-data.tsx`（テーブルの選択欄つき）
  - ほかの面: ミニアプリの画面の `` ```data_grid ``（保存ビューの束縛）・shiki script の `Shiki.data.*`・チャットの data ツール（Task 15.7）
  - Phase 15 は #903 で入ったばかりで、通しで使った実績がない。キャンペーンの主軸として検証する。
- **欠けているもの: データの変更を契機にワークフローを起動するイベントトリガ（GAP-07）。**
  語彙（`data.record.created` / `data.record.updated` / `data.transition`）は予約されているが、`EventSource::available_stage_a` は `storage.write` しか有効にしていない（`crates/workflow-engine/src/vocab/mod.rs:73`）。配線もなく、Task 15.7 の範囲にも入っていない。
  このため「申請が作られたら承認者に知らせる」「承認済みになったら台帳へ写す」のような、テーブル画面の操作から後続の処理を始めるアプリが組めない。
- パレットでは「業務データ」カテゴリ（「データを調べる」「データを追加」など）として出ている。ほかの画面は「テーブル」と呼んでいて、名前が揃っていない（GAP-38）。

## 2. 状況の一覧

| 区分 | 件数 | 内訳 |
| --- | --- | --- |
| 既知の不具合（human 報告） | 5 | BUG-1〜5。原因はすべて特定済み（§3） |
| 調査で見つけた不具合 | 20 | BUG-6〜25（§4） |
| 不足機能 | 41 | GAP-01〜41（§5）。P0 が 7 件 |
| 新機能 | 2 | FEAT-1（決定モデル）・FEAT-2（アプリエディタ）（§6） |
| 実践シナリオ | 12 | APP-01〜12（§7）。今の機能で組めるのは 2 本（03・08。日付や重複対策は script 頼み）、回避策つきで 5 本、一部だけ 1 本、不可 4 本 |
| human 判断待ち | 20 | D-1〜20（§8） |

## 3. 既知の不具合（human 報告）

| ID | 内容 | 原因（要約） | 状態 | PR |
| --- | --- | --- | --- | --- |
| BUG-1 | ワークフローの変数が機能せず、文字列がそのまま埋め込まれる | 4 つの欠陥の重なり（下記） | 修正中 | |
| BUG-2 | ノードを DnD で動かすと連続的に動かず、ドロップ位置へ瞬間移動する | 制御モードでドラッグ中の位置を捨てている＋座標の無いノードがあると全体を平行移動する | 修正中 | |
| BUG-3 | Shiki 内で AI にワークフロー（アプリ）を作らせる機能が十分に働くか不明 | AI が読む唯一の仕様（ツールの説明文）が誤っており、params の形も教えていない。アプリ（画面・テーブル）は作れない | 修正中（説明文） | |
| BUG-4 | LLM 呼び出しノードで LLM を選びにくい | モデル欄が自由入力で、照合先も組織の許可リストではない | 修正中（UI） | |
| BUG-5 | CSV 集計ノードでファイルを選べないように見える | `csv.*` の 3 ノードだけ設定フォームが未登録 | 修正中 | |

### BUG-1 変数が文字列のまま埋め込まれる

設計（ir.md §3）では、変数は `$from` 参照（パラメータの任意の位置に置ける・解決できなければ step を `expr_resolve_error` で失敗させる）と、`$template` の `{name}` 差し込み（`{{` `}}` は波括弧そのもののエスケープ）で表す。実装とのずれは 4 つある。

1. **AI 生成ツールが誤った記法を教えている。**
   `crates/chat/src/workflow_tool.rs:77` の説明文は、差し込みを `{{変数}}` と教えている。
   エンジン（`control/eval.rs:34-41`）は `{{` を `{` へのエスケープとして先に処理するので、実行すると `{変数}` という文字がそのまま出る。
   保存時検証 V5（`v5_dataflow.rs:53-63`）は `$template` の形しか見ないので、エラーも警告も出ない。
   同じ説明文は `$from` を `<祖先ノードid>`（正しくは `nodes.<id>.output`）、エッジを `"port":"next"`（正しくは `from_port`・既定 `out`）と書いており、V1 で拒否される。
2. **リテラルの中に入れ子にした `$from` / `$template` が、data の 3 ノードでしか展開されない。**
   ValueExpr の読み込み（`ir/expr.rs:61-80`）は最上位だけを値式として読み、`resolve_value`（`eval.rs:19`）はリテラルを再帰しない。
   入れ子を展開する `bind_values`（`nodes/capability_data.rs:32-55`）を使うのは `data.query`・`data.record.create`・`data.record.update` だけ。
   `http.request` の body、`script.run`・`skill.invoke`・`workflow.start` の input、`storage.write` の content、`data.bulk_upsert` の rows、`control.map` の items では `{"$from":…}` が生の JSON のまま渡る。
   V5（`v5_dataflow.rs:36-84`）は入れ子の参照を再帰的に検査して合格にするので、保存時と実行時で意味が食い違う。
3. **解決の失敗が黙って空文字・既定値になる。**
   `resolve_template`（`eval.rs:48-52`）は vars に無い `{name}` も、解決できない var も空文字にする（ir.md:121 の「黙って null を流さない」に反する）。
   任意項目では、`storage.write`・`csv.write` の folder がルートに、`script.run`・`skill.invoke` の input が run 入力全体に、`path_suffix` が空文字に、黙って置き換わる（BUG-13）。
4. **UI に差し込みの手段がない。**
   `ValueExprInput`（`value-expr-input.tsx:30-36,163-224`）の既定は「固定値」で、文字列中に変数を差し込むボタンがない。
   テンプレートに切り替えても、差し込み名の既定は `v1`、参照先の既定は入力全体（path なし）で、path は JSON Pointer の手打ち。
   入力項目（input_schema）を定義する UI も、前のノードの出力フィールドの候補もない（GAP-12）。

既存テストは `$template` とリテラル内の入れ子参照を一度も使っていない（`crates/workflow-engine/tests`・`crates/api/tests`・`web/e2e` で 0 件）ので、どの経路も捕まえられなかった。

**修正方針**

- エンジン: 深い解決を 1 か所（`resolve_deep`）にまとめ、ValueExpr 型の全フィールドで使う。String 型のフィールド（url・secret・skill など）にはかけない（PIT-36 の型防御）。テンプレートと任意項目の解決失敗は `expr_resolve_error` で失敗させる。
- 保存時検証: V5 で vars の中の `$from`、プレースホルダと vars の対応、`$from` の path の形式を検査する。vars のキーを `{{key}}` で囲んだものを警告する。V7 で `MAX_TEMPLATE_VARS` を強制する。
- AI の説明文: 例を実装に合わせ、例の IR を V1〜V7 に通すテストで固定する。
- UI: 「差し込む」ボタン、`{{x}}` の検出と変換、input_schema の編集 UI、前のノードの出力の候補。

### BUG-2 ノードの DnD が連続的に動かない

1. **主因**: キャンバスは nodes を IR・layout・選択状態から毎回作り直す制御コンポーネントだが、`onNodesChange` がドラッグ中（`dragging=true`）の位置変化を捨て、離したときの 1 回だけを layout に反映している（`canvas.tsx:149-159`）。
   @xyflow/react 12.8.6 は制御モードでは自分でノードを動かさないので、ドラッグ中はノードもエッジも止まったままになり、離した瞬間に最終座標へ飛ぶ。
2. **副因**: 座標が保存されていないノードが 1 つでもあると、dagre で全体を配置し直し、保存済みノードのずれの平均だけ全ノードを平行移動する（`auto-layout.ts:41-51`）。
   AI が作ったワークフローには layout が無いので、1 ノードを動かすとグラフ全体が同じだけ瞬間移動する。

**修正方針**: React Flow の公式の制御パターン（手元の state に `applyNodeChanges` で反映し、reducer へは確定した移動だけを送る）に直す。開いた時点で全ノードの座標を手元で補い、平行移動を起こさない。
エッジの選択・削除と削除の Undo（BUG-9）も同じ PR で直す。

### BUG-3 AI によるワークフロー（アプリ）作成

AI がワークフローを作る経路は、チャットの `emit_workflow` / `read_workflow` の 1 本。保存 API と同じ V1〜V7 を通し、失敗したら `is_error` を返して直させるループは動いている。問題は次のとおり。

- **説明文の誤り**（BUG-1 の 1）。LLM は例に従うので、ほぼ確実に誤る。
- **ノードの params と出力の形、使えるモデル ID を LLM に渡していない。** カタログ（単一定義）には type・ラベル・説明・ポート・スコープしか無い。
- **保存時検証の穴。** ポートの存在（BUG-7）、vars の中の参照、プレースホルダと vars の対応を見ていない。誤った IR が「保存成功」し、実行時に下流が黙って skip されたり、変数が空文字のまま埋め込まれたりする。
- **評価が無い。** e2e と結合テストは stub が出す 1 ノード・エッジなしの固定 IR だけで、実 LLM で作れるかを誰も測っていない（`eval/` にあるのは tool-search だけ）。
- **アプリの残りを作れない。** Yui `app` の書き方を AI に教えておらず、テーブル・保存ビュー・ミニアプリ本体を作るツールも、エディタ内の AI も無い（GAP-29〜33）。
- チャットの手数上限（既定 6）では、作成と自己修正の余地が小さい。V1（serde）のエラーが 1 往復 1 件しか返らないことも効いている。

**修正方針**: まず説明文と検証の穴を直し、params の形をカタログから生成して説明文に載せる。その上で評価ハーネス（`eval/app-builder/`）を作り、修正前後の保存成功率と「黙って壊れる率」を数値で比べる。アプリの残り（画面・テーブル・試運転）は D-10 の判断後。

### BUG-4 LLM を選びにくい

- **UI**: `LlmInvokeForm` のモデル欄はただの自由入力（`forms-basic.tsx:130-143`、placeholder は架空の「例: gpt-x-mini」）。
- **保存時の照合先がずれている**: V3 は組織の許可リストではなく、デプロイ設定 `config.llm.models` の id と照合する（`v3_vocab.rs:54-74`・`save.rs:78-84`）。一覧が空の Compose 構成では照合そのものを飛ばすので、環境によって通る値が変わる。
- **省略時の解決**: モデルを省略すると、組織の既定や Auto ではなくデプロイの既定で動く（`ports.rs:215-219`）。チャットとは規則が違う（D-5）。
- **エラーの位置**: V3 のエラーに path が無いので、モデル欄の下ではなくパネル上部の帯に出る。候補も示さない。
- 再利用できる部品: `GET /llm/models`（組織の設定で絞った一覧）、`useLlmModels()`、チャットの `ModelList`（検索・プロバイダ見出し・国外処理バッジ・キーボード操作つき。未 export）。

**修正方針**: `ModelList` を `web/src/components/llm/` へ切り出してピッカーにする。照合先を組織の許可リストに直し、エラーをモデル欄に紐づけて候補を添える。

### BUG-5 CSV 集計ノードでファイルを選べない

- **原因は UI の欠落**。`csv.query`・`csv.patch`・`csv.write` は params 契約もエンジンも揃っているが、設定パネルのフォーム表（`config-panel/index.tsx:48-70`）に登録が無い。右パネルには「このブロックに設定はありません」とだけ出る。Stage A で使える 24 種のうち、この 3 種だけが漏れている。
- 既定の params も `{}` なので、置いた直後から V1 で「項目 'file' がありません」になり、直す欄が無いためノードを消すまで保存できない。
- ファイルを選ぶ部品（`DrivePicker`・`FolderPicker`）は既にあるが、ノード設定では使われていない。`storage.read/write/list` も UUID を手で打つ欄のまま。
- 本番運用で効く関連の問題: csv.* の同意計画が無い（BUG-10）、ファイル保存イベントの入力に file_id が無い（BUG-8）、`csv.patch` に渡す版（base_rev）をどの出力からも取れない（GAP-40）。

**修正方針**: `forms-files.tsx` を新設し、ファイル欄（ドライブから選ぶ／前の結果から指定するの切替）・SQL 欄（列名の表示と試し実行）・保存先フォルダ欄を作る。storage.* の欄も同じ部品に置き換える。

## 4. 調査で見つけた不具合

| ID | 優先度 | 内容 | 根拠 | 状態 | PR |
| --- | --- | --- | --- | --- | --- |
| BUG-6 | P0 | `agent.invoke` が自然文の指示を Python コードとして実行する。パレットと右パネルは「AI エージェント」「添付の資料を確認して要点をまとめて」と見せているので、その通りに書くと必ず失敗する | `api/src/workflow_runtime/ports.rs:267-326` | D-4 待ち | |
| BUG-7 | P0 | V2 が `from_port` の存在を検証しない。存在しないポートのエッジが保存でき、実行時は dead edge になって下流が黙って skipped になる（run は成功扱い） | `ir/validate/v2_graph.rs:67-95`・ir.md §8 V2 | 修正中 | |
| BUG-8 | P0 | ファイル保存トリガの run 入力にファイル ID・名前・op が無い（outbox の `{kind, blob_sha256, size, parent_id}` そのまま）。削除でも発火する | `api/src/workflow_runtime/mod.rs:438-456` | 修正中 | |
| BUG-9 | P1 | エッジを選択・削除できない。エッジをクリックしてから Delete を押すと直前に選んでいたノードが消える。1 回の削除で Undo が 2 段に分かれ、座標も戻らない | `canvas.tsx:106-127,170-201`・`ir-state.ts` | 修正中 | |
| BUG-10 | P1 | csv.* の同意計画が無く、schedule／event の run は権限不足で失敗する（有効化は成功してしまう） | `registration/consent.rs` | 修正中 | |
| BUG-11 | P1 | 保存時のモデル照合が組織の許可リストを見ない。Compose では照合自体を飛ばす | `v3_vocab.rs:54-74`・`save.rs:78-84` | 修正中 | |
| BUG-12 | P1 | モデル省略時にデプロイ既定で動き、組織の既定・Auto を無視する（skill・ミニアプリの AI も同じ） | `ports.rs:215-219`・`skill.rs:92`・`app-gateway/src/routes/ai.rs:133-136` | D-5 待ち | |
| BUG-13 | P1 | 任意項目の参照が解決できないと黙ってフォールバックする（folder→ルート、input→run 入力全体、path_suffix→空文字、workflow.start の input→null） | `capability.rs:88-92,244-248,368-372`・`script.rs:61-65`・`skill.rs:42-46`・`capability_ai.rs:139-145` | 修正中 | |
| BUG-14 | P1 | 条件の大小比較で型が合わないと黙って false になる（ir.md §3.3 は `expr_type_error`）。固定値の数字は文字列として保存されるので、金額の比較が黙って逆に流れる | `control/eval.rs` の `compare_ordered` | 修正中 | |
| BUG-15 | P1 | `llm.invoke`・`agent.invoke` の既定 params が入力全体（オブジェクト）を参照し、置いたまま実行すると「prompt が解決できません」で落ちる | `ir-state.ts:97-100` | 修正中 | |
| BUG-16 | P2 | 「条件を追加」の既定の左辺が `path: "/"`（空文字キー）で、eq は常に偽・neq は常に真 | `forms-control.tsx:212` | 修正中 | |
| BUG-17 | P1 | ミニアプリの任意の skill ピンが壊れると、アプリ全体が「見つかりません」になる | `crates/gui/src/miniapp_store.rs::resolve` | 未着手 | |
| BUG-18 | P1 | 共有で editor にした人にミニアプリの編集ボタンが出ない（UI が owner だけに出す） | `apps/[id]/page.tsx:48` | 未着手 | |
| BUG-19 | P1 | ボタンの run が終わってもミニアプリの表（data_grid）が読み直されない | `apps/[id]/page.tsx`・`data-grid.tsx` | 未着手 | |
| BUG-20 | P2 | 待ち（wait）の run が「実行中」のまま表示され、開いている間 3 秒ごとに読み直し続ける | `RunStatus`・`apps/[id]/page.tsx` | 未着手 | |
| BUG-21 | P2 | 必須項目の欠落エラーが欄に写らない（サーバは日本語化し、web は英語の原文しか照合しない） | `validation.ts:37-50`・`params/mod.rs:97-102` | 修正中 | |
| BUG-22 | P1 | `dev-up.sh` がカーソル鍵（`SHIKI__DATA__EPHEMERAL_CURSOR_KEY`）と tabular runner のパスを渡しておらず、native 起動が bail／csv.* が失敗する | `.claude/skills/pr/scripts/dev-up.sh:512-585,631` | 回避策あり（§9） | |
| BUG-23 | P1 | `storage.read` がバイナリ文書（PDF・docx・xlsx）を不可逆に文字化けした text で返す | `capability.rs`（`from_utf8_lossy`） | GAP-14 で扱う | |
| BUG-24 | P2 | data_grid のページ送りのたびに resolve と監査（`miniapp.resolve`・`artifact.read_via_bundle`）が走る | `crates/gui/src/miniapp_data.rs` | 未着手 | |
| BUG-25 | P2 | トリガの座標を添字（`trigger:${i}`）で保存しており、前のトリガを消すと別のトリガに付け替わる | `workflow-editor.tsx` | 修正中 | |

## 5. 不足機能

GAP-01〜05・18・20 は §3〜§4 の不具合と同じものなので、ここでは対応だけを示す（01・02→BUG-1、03→BUG-6、04→BUG-8、05→BUG-5、18→BUG-13・14、20→BUG-4）。

### 5.1 業務アプリの部品（シナリオ調査から）

| ID | 優先度 | 内容 | 主に効く APP | 状態 |
| --- | --- | --- | --- | --- |
| GAP-06 | P0 | ワークフローから人に知らせる手段が無い（`notify.send` は予約、通知一覧の画面はモック） | 01・02・05・06・09・11・12 | D-2 待ち |
| GAP-07 | P0 | データ変更トリガ（作成・更新・状態遷移）と data の `wait(event)` が無い（roadmap では Task 13.14） | 01・02・05・11・12 | D-1 待ち |
| GAP-08 | P1 | 承認待ち（`human.approval`）・期限・多段承認が無い | 01・05 | D-1 待ち |
| GAP-09 | P1 | ミニアプリの画面に run の結果（出力・失敗理由）が返らない。アプリだけを共有された人に結果を返す経路も無い | 02・07・11・12 | D-3 待ち |
| GAP-10 | P1 | Yui app の部品の不足（ファイル添付・ユーザー／部署・テーブルの行の選択） | 01・04・05・07・11 | D-19 待ち |
| GAP-11 | P1 | ミニアプリ編集画面で保存ビュー（data_views）を束ねられない。ワークフローの版を上げる操作も無い | 全部 | 未着手 |
| GAP-12 | P1 | エディタに input_schema（入力項目）の編集 UI が無い。画面の `as` 名との突き合わせも無い | interactive 全部 | 修正中 |
| GAP-13 | P1 | `llm.invoke` の構造化出力（`output_schema`）が無い（ir.md §7.3 にあるが未実装） | 02・04・05・07・09・10 | 未着手 |
| GAP-14 | P1 | PDF・Word の本文を読めない（`doc.read` を `rag::SearchService::read_document` の上に作る案） | 04・05・07・09 | 未着手 |
| GAP-15 | P1 | 発火時刻・相対日付・週月の丸めが無い（schedule のペイロードは Null） | 03・06・08・11 | 未着手 |
| GAP-16 | P1 | 自然キーでの upsert と加算更新が無い | 06・08・10 | 未着手 |
| GAP-17 | P1 | 実行した本人を `$from` で参照できない（`run` は `{run_id, tenant_id}` だけ） | 01・11 | 未着手 |
| GAP-19 | P1 | `http.request` ノードに Content-Type・任意ヘッダが付かない（script の経路は付く） | 外部通知 | 修正中（Content-Type） |
| GAP-21 | P2 | メール受信・Webhook 受信のトリガが無い | 02 | 未着手 |
| GAP-22 | P2 | PDF・Excel・Word の帳票を生成できない | 03・06・08 | 未着手 |
| GAP-23 | P2 | 200 行を超えるページ送りのループが無い。`storage.list` は 100 件で打ち切り | 03・10 | 未着手 |
| GAP-24 | P2 | ノーコードのデータ加工（`transform.*`）が無い | 10 | 未着手 |
| GAP-25 | P2 | 真偽・連番・既定値・複合ユニークの型が無い | 01・03 | 未着手 |
| GAP-26 | P2 | data_grid の行操作とグラフのデータ束縛が無い | 08・12 | 未着手 |
| GAP-27 | P2 | Shiki API と仕様の食い違い（`Shiki.data` に delete・bulkUpsert が無い、`Shiki.rag.search` の戻り値、`Shiki.http.request` の headers が無視される） | script を使うもの | 未着手 |
| GAP-28 | P2 | `llm.invoke`・`http.request` に effect_journal が効かない（リース失効で二重課金・二重送信） | 全部 | 未着手 |

### 5.2 AI による作成（BUG-3 の続き）

| ID | 優先度 | 内容 | 状態 |
| --- | --- | --- | --- |
| GAP-29 | P0 | ノードの params・出力の形を LLM が知る手段が無い（カタログから生成して説明文に載せる。`describe_node` ツールの案も） | 修正中 |
| GAP-30 | P0 | AI 作成の評価ハーネスが無い（`eval/app-builder/`。課題集＋採点: 保存時検証を通るか・実行して期待結果になるか） | 未着手 |
| GAP-31 | P1 | 既存ワークフローを探すツール（`list_workflows`）が無い。新しい会話から「経費フローを直して」と頼めない | 未着手 |
| GAP-32 | P1 | AI がテーブル・保存ビュー・ミニアプリ本体を作れない。Yui app の書き方も教えていない | D-10 待ち |
| GAP-33 | P1 | ワークフローエディタ・ミニアプリエディタに「AI に依頼」パネルが無い（Task 10.13 の「エディタ内 AI」・UI 文言は「AI 編集で変更できます」と案内している） | D-10 待ち |
| GAP-34 | P1 | AI が作ったワークフローを試運転して確かめる手段が無い（閉ループが無い） | D-10 待ち |
| GAP-35 | P1 | V1（serde）のエラーが 1 往復 1 件・path なしで返る。V4 がテーブル・フォルダ id の実在を見ない（ir.md §8 V4） | 未着手 |

### 5.3 ミニアプリ・エディタの操作感

| ID | 優先度 | 内容 | 状態 |
| --- | --- | --- | --- |
| GAP-36 | P1 | ミニアプリの下書き・試し実行・公開の区別が無い（保存した瞬間に全利用者の画面が変わる） | D-16 待ち |
| GAP-37 | P1 | 利用者の権限の事前確認が無い（共有しても、表とワークフローの書き先の権限は本人の ReBAC で評価される。足りなくても開いた後か run の失敗で初めて分かる） | 未着手 |
| GAP-38 | P2 | 呼び名の不統一（パレットの「業務データ」とほかの画面の「テーブル」） | 未着手 |
| GAP-39 | P2 | ワークフローエディタの操作: 複数選択・コピー／貼り付け・接続の妥当性の事前検査（`isValidConnection`）・エッジの付け替え・キーボード操作（Esc・Ctrl+A など）・未接続ノードが入口として走る落とし穴 | 未着手 |
| GAP-40 | P1 | ファイルを扱うフローの部品: `csv.patch` の base_rev を出力から取れない、`csv.query` の rows が文字列の 2 次元配列だけ、xlsx・Shift_JIS 非対応、有効化ダイアログでファイルを選べない | 一部修正中 |
| GAP-41 | P2 | 誤りの文言が利用者向けでない（ミニアプリの 409 が「API 409」、503 が「結果を確かめられませんでした」、入力の誤りが `/amount` のような JSON Pointer、ワークフローの LLM エラーが英語の原文） | 一部修正中 |

## 6. 新機能

### FEAT-1 決定モデル（Decision Model）による決定的な推論ノード

**Jev とは**: TypeSafe AI が 2026-09-15 に早期提供を始めた非生成の「System One」決定モデル。

- 入力: アプリの状態（state）と、型付きの質問（`choice`＝選択肢 1〜255・`score`＝順序のある 2〜10 段階・`noul`＝はい／いいえ）。
- 出力: 各質問の答えと確率・信頼度。生成ではなく並列に評価し、70〜500ms で返る（メーカー公表値）。型から外れた値は返さないが、判断そのものは誤り得る。
- 提供経路: TypeSafe API（`POST /v1/systemone`・版固定の `jev-1.13.0` を推奨）、Vercel AI Gateway（`typesafe-ai/jev`・AI SDK の `experimental_decide`）、DigitalOcean、AIMLAPI など。
- 価格: 入力 $0.042／100 万トークン、出力は無課金（メーカー公表値）。
- 未確認: 同じ入力に同じ出力を返すか（公式の記載なし）、日本語の精度、データ保持の条件、自己ホスト。米国での処理になる。
- 苦手: 数え上げ・算術・日付の比較、大きくノイズの多い state。

**shiki への組み込み案**（トレイト境界の変更を含むので D-6 で human が決める）

- llm-gateway に新トレイト `DecisionProvider` を足し、`LlmGateway::decide` をチョークポイントにする（カタログ → 組織の許可 → 上限 → 実行 → 会計の順は `stream` と同じ）。非生成モデルを別トレイトにする前例は `EmbeddingProvider`・`Reranker`。
- アダプタは 2 つ: TypeSafe 互換の HTTP アダプタと、通常の LLM に決定を真似させる `llm_decider`（温度 0・JSON を厳密に検証・`calibrated=false`）。後者はオンプレとフォールバックを兼ねる。
- モデルカタログに種類（chat／decision）を持たせ、チャットのモデル選択と `llm.invoke` から決定モデルを外す。
- ワークフローに新ノード（仮称 `ai.decide`）を足す。params は `model`・`state`（値式）・`questions`（リテラル）・`route`（質問と信頼度しきい値でポートを選ぶ）・`fallback`・`cache`。v1 は既存の branch／switch で振り分ける。
- 決定性は、版の固定と、テナント・org 内のキャッシュ（キー＝版＋正規化した state と questions）で担う（第 2 段）。
- 先に、仕様にあるのに未実装の `llm.invoke` の `output_schema`・`effort`・`temperature` を入れる（GAP-13）。分類系のフローの品質と、決定のフォールバックの両方に効く。

**ユースケース**（シナリオとの対応）

| # | 用途 | 質問の型 | 後段 |
| --- | --- | --- | --- |
| 1 | 問い合わせの振り分け（APP-02） | choice: 請求／不具合／使い方／解約／その他 | switch で担当チーム |
| 2 | 問い合わせの優先度（APP-02） | score: 低〜緊急の 4 段階 | しきい値で即時通知 |
| 3 | 経費の規程違反・不正の疑い（APP-01） | noul: 規程違反の疑いがあるか | 信頼度が低ければ要確認テーブルへ |
| 4 | 承認経路の判定（APP-01） | choice: 上長のみ／部長まで／経理確認 | 承認者の決定 |
| 5 | 書類選考の足切り（APP-05） | score: 必須要件の充足度 | 自動で見送りにはせず要確認へ |
| 6 | 発注の要否（APP-06） | noul: 今日発注すべきか | 計算は script・判断だけを決定モデル |
| 7 | 貸出の可否（APP-11） | noul: 役職×備品区分×期間で可か | 理由を画面へ |
| 8 | ヘルプデスクの自動回答の可否（APP-12） | noul: 検索結果だけで答えられるか | 答えられなければエスカレーション |
| 9 | ガードレール（全般） | noul: 個人情報・機密を含むか | 外部送信の前に止める |
| 10 | 重複の検出（APP-04・09） | noul: 既存の行と同じ案件か | 重複なら更新に回す |

数え上げや算術が苦手なので、金額の計算や日付の比較は script や data のクエリで済ませ、決定モデルには判断だけを任せる構成にする。

### FEAT-2 アプリエディタの使いやすさ

- yui 側の DnD エディタ（y-ui PR #3「app の画面をドラッグ＆ドロップで組み立てるエディタとチェッカー」）は 2026-10-25 にマージ済みで、`@imanect/yui` 0.9.0 として公開済み。
- shiki 側の組み込みは **PR #902**（Issue #901・draft）にある。中身は「組み立てる」タブ、画面を段に分ける部品、入力の候補、Rust 版パーサの追従、E2E。CI は Coverage（`crates/chat` の `threads_it`・#902 の変更外）以外は通っている。
- 本キャンペーンでは #902 を前提にする（D-8）。#902 の上に残る課題:
  - 「組み立てる」が別名の見出し（form・アプリ・フォーム・`app:`）の app を開けない（`screen-blocks.ts` の判定が正規表現だけ）
  - 警告が 1 つでもあると「組み立てる」で開かない（LLM が書いた app は警告が付きやすい）
  - 表（保存ビュー）の段が無い（GAP-11）
  - 選択肢の「送る値」を直せない
  - TS 版と Rust 版の課題集に版の記録が無く、web だけ yui を上げても CI が気づかない

## 7. 実践シナリオ（APP-01〜12）

| ID | 業務（部門） | 起動 | 主なノード | 今の機能で組めるか | 主に効く GAP |
| --- | --- | --- | --- | --- | --- |
| APP-01 | 経費精算：申請 → 上長承認 → 経理台帳（経理・全社） | interactive ＋ schedule（15 分） | data.record.create・FSM・data.query・map・data.transition | 回避策つきで可 | 06・07・08・10・17 |
| APP-02 | 問い合わせの一次仕分けと担当割当（CS） | interactive（受付が貼り付け） | llm.invoke ×2・script・data.update・rag.search | 回避策つきで可 | 01・06・09・13・21 |
| APP-03 | 日報の収集と週次集計レポート（営業） | interactive ＋ schedule（月 9:00） | data.query（集計）・llm・storage.write | 可（日付は script） | 06・15・22・23 |
| APP-04 | 請求書 PDF の読み取り → 支払台帳（経理） | event（フォルダ保存） | storage.read・llm・script・data.create | 不可 | 04・14・13・06 |
| APP-05 | 採用応募の書類スクリーニング（人事） | interactive | data.query・llm・script・data.transition | 一部（履歴書を読めない） | 10・14・13・08・06 |
| APP-06 | 在庫の発注点アラートと発注書の下書き（小売・製造） | interactive ＋ schedule（毎朝） | data.update（rev）・data.query・bulk_upsert・csv.write | 回避策つきで可 | 16・06・15 |
| APP-07 | 契約書の条項チェック（法務） | interactive | storage.read・rag.search・llm・storage.write | 不可（docx/pdf を読めない） | 14・09・10・03 |
| APP-08 | 週次 KPI ダッシュボード（経営企画） | schedule（月 7:00） | data.query（集計）・script・bulk_upsert・llm | 可（重複対策は script） | 15・16・26 |
| APP-09 | 議事録 → タスク化（全社・PMO） | event（フォルダ保存） | storage.read・llm・script・map・data.create | 不可 | 04・13・17・06 |
| APP-10 | 顧客アンケート（CSV）の分析（マーケ・CS） | event（CSV 保存） | csv.query・bulk_upsert・map・llm・data.query | 不可 | 04・05・16・24 |
| APP-11 | 備品の貸出管理（総務・情シス） | interactive ×2 ＋ schedule | data.query・data.transition（rev）・data.create | 回避策つきで可 | 09・17・10・06・15 |
| APP-12 | 社内 FAQ ヘルプデスク（情シス・総務） | interactive ×2 | rag.search・branch・llm・data.create・transition | 回避策つきで可（回答を画面に出せない） | 09・06・26 |

「不可」の理由は 3 種類: ファイル保存トリガのペイロードにファイル ID が無い（APP-04・09・10）、PDF や Word の本文を読めない（APP-04・07）、CSV ノードに設定画面が無い（APP-10）。

**着手順**: P0 の土台（BUG-1・2・5・7・8）を直したあと、interactive 中心で欠けが少ない APP-12 → APP-11 → APP-01 → APP-02 の順に作って使う。次に schedule 系（APP-03・06・08）、ファイル起点（APP-04・09・10）、APP-05・07 の順。

### APP-01 経費精算（申請 → 上長承認 → 経理台帳）

- **想定**: 社員が経費を申請し、上長が承認する。経理は承認済みを台帳に計上して「計上済み」にする。月 300 件規模。
- **画面**:

  ````text
  ```ui
  app 経費の申請
  件名 = text 必須 as title
  金額 = number 1..1000000 円 必須 as amount
  区分 = choice 交通費=travel | 出張費=trip | 交際費=entertain | 備品=supplies as category
  利用日 = date 必須 as used_on
  内容 = textarea as note
  button 申請する -> workflow 経費申請
  ```
  ```data_grid
  自分の申請
  ```
  ````

- **テーブル**:
  - `expense_claims`: title / amount / category select / used_on date / note / receipt file_ref / approver user_ref / status。状態は submitted → approved / rejected → posted。承認・却下の actor は `approver == $user.id`、計上の actor は経理ロール。行ポリシは「作成者か approver か経理」。
  - `approvers`: member user_ref unique / manager user_ref / director user_ref。
  - `expense_ledger`: claim record_ref / amount / account select / posted_on date。
- **ワークフロー**:
  - A「経費申請」（interactive）: script.run で規程を判定（交際費が 1 人 5,000 円超で要注記、10 万円以上は部長承認）→ data.query で上長を引く → data.record.create。
  - B「経費計上」（schedule `*/15 * * * *`・経理ユーザーの委譲）: data.query(status=approved) → control.map で台帳へ data.record.create → data.transition（→ posted）。
- **成功判定**: (1) 申請すると submitted の行が 1 つでき、approver が正しい上長になる (2) 上長以外には承認メニューが出ない（遷移は 403） (3) 承認後 15 分以内に台帳に 1 行でき、申請は posted になる (4) B を再実行・同時実行しても台帳に重複が出ない (5) 経理以外は posted へ遷移できない。
- **不足**: 承認後の起動（GAP-07・08）、通知（06）、領収書の添付（10）、本人 id の参照（17）、規程違反を画面に返すこと（09）。規程判定は FEAT-1 の第一候補。

### APP-02 問い合わせの一次仕分けと担当割当

- **想定**: 顧客からのメールを受付担当が画面に貼り付ける（メール受信のトリガが無いための代替）。AI が分類・優先度・要約・返信下書きを作り、担当チームに割り当てる。
- **画面**: 顧客名 / メール / 件名 / 本文 textarea / 製品 choice → `button 仕分ける -> workflow 問い合わせ仕分け`。data_grid は「未対応の問い合わせ」。
- **テーブル**: `inquiries`（category: 請求／不具合／使い方／解約／その他、priority: 高／中／低、summary、draft_reply、assignee_team role_ref、status: new → triaged → answered → closed）、`routing_rules`（category unique → team）。
- **ワークフロー**: data.record.create → llm.invoke（分類 JSON）→ script.run（JSON.parse・選択肢外は「その他」）→ data.query（割当表）→ data.record.update → data.transition（→ triaged）→ rag.search → llm.invoke（出典つきの返信下書き）→ data.record.update。LLM ノードは on_error=continue で「要手動仕分け」へ。
- **成功判定**: (1) 典型的な問い合わせ 20 件で分類の一致率 80% 以上 (2) 選択肢外の値で 422 にならない (3) 担当チームが割当表どおり (4) 下書きに社内 FAQ の出典が付く (5) LLM が失敗しても行が残り手動扱いになる。
- **不足**: 構造化出力（13）、通知（06）、画面への分類結果（09）、メール受信（21）。優先度は FEAT-1 の候補。

### APP-03 日報の収集と週次集計レポート

- **画面**: 日付 / 訪問件数 count / 商談件数 count / 見込み額 / 所感 → `button 提出する -> workflow 日報提出`。data_grid は「今週の自分の日報」。
- **テーブル**: `daily_reports`（行ポリシは作成者と部長）、`weekly_reports`（week_start / summary / report_file / totals）。
- **ワークフロー**: 日報提出は同じ日付の行を照会して update か create（複合ユニークが無い）。週次（`0 9 * * 1` Asia/Tokyo）は script.run で先週の範囲 → data.query（group_by `_owner`・sum）→ data.query（所感）→ llm.invoke（要約）→ storage.write（`週報-YYYY-MM-DD.md`）→ data.record.create。
- **成功判定**: (1) 月曜 9:00 に .md が保存される (2) 集計が手計算と一致 (3) 未提出者がレポートに出る (4) catchup で再発火しても二重にならない。
- **不足**: 発火時刻と日付（15）、社員一覧の参照、通知（06）、帳票（22）、200 件超の所感（23）。

### APP-04 請求書 PDF の読み取り → 支払台帳

- **入力**: 「請求書受付」フォルダへ PDF を保存（event storage.write）。画面の data_grid は「要確認の請求書」。
- **テーブル**: `vendors`（name unique・bank_account はマスク）、`payables`（invoice_no unique / vendor record_ref / amount / tax / due_date / file file_ref / status: 要確認 → 承認済 → 支払済）。
- **ワークフロー**: storage.read（`$from trigger /file_id`）→ PDF の本文（doc.read が要る）→ llm.invoke（抽出）→ script.run（検算・日付の正規化）→ data.query（取引先）→ data.record.create（unique 違反は重複扱い）。
- **成功判定**: (1) PDF を置くと 2 分以内に 1 行 (2) サンプル 10 枚中 9 枚で金額・期日・請求番号が一致 (3) 再保存しても重複しない (4) 取引先が一致しなければ「要確認」で止まる。
- **不足**: ファイル ID（BUG-8）、PDF の本文（14）、構造化出力（13）、通知（06）。

### APP-05 採用応募の書類スクリーニング

- **画面**: 氏名 / 応募職種 / 経験年数 / 志望動機。履歴書は部品が無いのでファイル ID を入れる → `button 登録して評価 -> workflow 書類選考`。
- **テーブル**: `job_requirements`（position unique / must / nice）、`applicants`（name はマスク / score / reason / status: 応募 → 書類通過 / 見送り…。見送りの actor は人事ロールだけ）。
- **ワークフロー**: data.record.create → data.query（要件）→ storage.read（PDF は読めない）→ llm.invoke（要件ごとの充足度。属性を評価に使わない指示）→ script.run（決定的な点数計算）→ data.record.update → branch（score ≥ 70）→ data.transition（→ 書類通過）。自動で見送りにはしない。
- **成功判定**: (1) 評価理由が要件の項目に紐づく (2) 同じ入力で点数が再現する (3) 自動で見送りにならない (4) 個人情報が LLM の記録に過剰に残らない（要確認）。
- **不足**: ファイル部品（10）、PDF（14）、構造化出力（13）、承認待ち（08）、通知（06）。足切りは FEAT-1 の候補。

### APP-06 在庫の発注点アラートと発注書の下書き

- **テーブル**: `suppliers`、`items`（sku unique / stock / reorder_point / lot_size / supplier / lead_time_days / shortage formula）、`purchase_orders`（status: 下書き → 発注済 → 入荷済）。
- **画面**: SKU / 数量 / 区分（入庫・出庫）→ `button 登録 -> workflow 入出庫`。data_grid は「発注点割れの品目」と「下書き発注」。
- **ワークフロー**: 入出庫（interactive）は data.query(sku) → script.run（新在庫・マイナス在庫チェック）→ data.record.update（expected_rev）→ branch → workflow.start。発注点チェック（毎朝 8:00）は data.query（shortage > 0）→ 未完了の発注 → script.run（ロット切り上げ）→ data.bulk_upsert → csv.write（発注書.csv）。
- **成功判定**: (1) 発注点を割った品目だけが下書きになる (2) 二重に発注しない (3) 同時に出庫しても在庫が壊れない（片方は rev 競合で失敗し、再操作で整合）。
- **不足**: 自然キーの upsert と加算更新（16）、通知（06）、日付計算（15）。

### APP-07 契約書の条項チェック

- **画面**: 契約種別（NDA／業務委託／売買）/ 相手方 / 契約書のファイル ID / 希望期限 → `button AI チェック -> workflow 契約チェック`。
- **テーブル**: `contract_reviews`（risk: 高／中／低 / findings / reviewer / status: 依頼 → AI 確認済 → 法務確認済）。チェックリストはドライブに置いて RAG で引く。
- **ワークフロー**: 本文（doc.read が要る）→ rag.search（種別のチェックリスト）→ llm.invoke（条項ごとに有／無／要注意と引用）→ script.run（リスク判定）→ storage.write（指摘レポート.md）→ data.record.create → data.transition。
- **成功判定**: (1) テスト契約 5 本に仕込んだ問題（損害賠償の上限なし・準拠法なし・自動更新・秘密保持の期間が無期限）を全部検出 (2) 指摘に条文の引用 (3) 閲覧権限の無いチェックリストを参照しない (4) 結果が依頼者の画面に出る。
- **不足**: docx/pdf の本文（14）、画面への結果（09）、ファイル部品（10）。

### APP-08 週次 KPI ダッシュボード

- **テーブル**: `deals`（stage: リード／提案／交渉／受注／失注 / close_date / sales / region）、`kpi_snapshots`（week_start / metric / region / value）。
- **ワークフロー**（月 7:00）: script.run（週の範囲）→ data.query（受注の group_by region・sum と count）→ data.query（商談数）→ script.run（受注率・前週比）→ data.bulk_upsert → llm.invoke（コメント）→ storage.write。
- **成功判定**: (1) 数値が手集計と一致 (2) 再実行しても snapshot が重複しない (3) 少人数の地域が suppressed として正しく表示される。
- **不足**: グラフのデータ束縛（26）、週・月の丸め（15）、自然キーの upsert（16）。

### APP-09 議事録 → タスク化

- **入力**: 「議事録」フォルダに .md を保存（event）。画面の data_grid は「自分のタスク」。
- **テーブル**: `tasks`（assignee user_ref / due date / meeting file_ref / excerpt / status: 未着手 → 進行中 → 完了・actor は担当者）、`members`（表示名 → ユーザー）。
- **ワークフロー**: storage.read（トリガのファイル ID）→ llm.invoke（タスクの JSON 配列）→ script.run（相対期日を日付に・名前を照合）→ control.map で data.record.create。
- **成功判定**: (1) サンプル議事録 3 本で抽出漏れ 1 件以内 (2) 担当が実在ユーザーに解決（できなければ空欄＋要確認） (3) 期日が date 型で入る (4) 同じファイルを再保存しても重複しない。
- **不足**: ファイル ID（BUG-8）、docx（14）、名前からユーザーへの解決、構造化出力（13）、通知（06）。

### APP-10 顧客アンケート（CSV）の分析

- **入力**: アンケートツールの CSV を「アンケート」フォルダへ置く（event）。
- **テーブル**: `survey_responses`（response_id unique / nps / comment / segment / sentiment / topics multi_select）、`survey_reports`。
- **ワークフロー**: csv.query → script.run（数値化・既存 id と照合）→ data.bulk_upsert → script.run（50 件ずつ束ねる）→ control.map → llm.invoke（感情とトピック）→ data.bulk_upsert → data.query（NPS 区分の集計）→ llm.invoke（要約）→ storage.write。
- **成功判定**: (1) 500 行が全件取り込まれる (2) NPS が手計算と一致 (3) 再取込で件数が増えない (4) LLM 呼び出しが 15 回以内。
- **不足**: ファイル ID（BUG-8）、CSV ノードの設定画面（BUG-5）、自然キーの upsert（16）、transform（24）。

### APP-11 備品の貸出管理

- **テーブル**: `equipment`（asset_no unique / borrower / due_date / status: 在庫 ⇄ 貸出中・修理中）、`loans`（out_date / due_date / returned_on）。
- **画面**: 貸出（資産番号・返却予定日）と返却（資産番号）の 2 ボタン。data_grid は「貸出できる備品」と「自分の貸出中」。
- **ワークフロー**: 貸出は data.query(asset_no) → branch（在庫か）→ data.transition（expected_rev）→ data.record.update（借り手）→ data.record.create（loan）。返却は data.transition（→ 在庫）→ loan を update。延滞（毎朝）は data.query（期日切れかつ未返却）。
- **成功判定**: (1) 2 人が同時に同じ備品を借りると片方だけ成功し、失敗した側に理由が出る (2) 返却で在庫に戻る (3) 延滞者に翌朝通知が届く。
- **不足**: 画面への結果（09）、本人 id（17）、レコード選択の部品（10）、「今日」（15）、通知（06）。

### APP-12 社内 FAQ ヘルプデスク

- **画面**: 質問 / カテゴリ（PC／アカウント／経費／総務）→ `button 質問する -> workflow 質問`。data_grid は「自分の問い合わせ」。
- **テーブル**: `helpdesk_tickets`（answer / sources / assignee role_ref / status: 自動回答 → 解決 / エスカレーション → 対応中 → 完了）。
- **ワークフロー**: rag.search（top_k 5）→ branch（スコアが閾値以上）→ llm.invoke（出典つき回答）→ data.record.create。答えられなければ data.record.create → data.transition（→ エスカレーション）。
- **成功判定**: (1) 既知の FAQ 10 問中 8 問に正しい出典つきで答える (2) 閲覧権限の無い文書を引用しない（別ユーザーで確かめる） (3) 答えられないときはエスカレーション (4) 回答が質問者の画面に出る。
- **不足**: 画面への結果（09・このアプリでは必須）、行ごとのボタン（26）、通知（06）。

## 8. human 判断待ち

| ID | 判断すること | 推奨 | 決定 |
| --- | --- | --- | --- |
| D-1 | データ変更トリガ（GAP-07・roadmap Task 13.14）と承認待ち（GAP-08）をこのキャンペーンに前倒しするか | 前倒しする（データテーブルが既定の保存先なので、無いと承認系のアプリが組めない） | |
| D-2 | `notify.send`（GAP-06）の配信先 | アプリ内通知（ベル＋一覧）から始め、メール・Slack は後 | |
| D-3 | ミニアプリの画面に run の結果を返す方式（GAP-09） | IR で「画面に返す出力」を宣言する。返すのは起動した本人の分だけ | |
| D-4 | `agent.invoke`（BUG-6）を (a) 実態どおり「Python を実行」に改名 (b) agent-core のツールループにつなぐ (c) パレットから外す | 当面 (c)、本来の姿は (b) | |
| D-5 | `llm.invoke` のモデルを省略したときの意味（BUG-12） | 組織の既定（Auto を含む）に従う。チャットと規則を揃える | |
| D-6 | FEAT-1 のトレイト境界（新トレイト `DecisionProvider`／`LlmProvider` に既定実装つきのメソッド／トレイトを変えず skill で PoC）とノード名（`ai.decide` など） | 新トレイト `DecisionProvider`・`ai.decide`。先に GAP-13（構造化出力）を入れる | |
| D-7 | FEAT-1 で信頼度がしきい値未満のときの扱い | 承認待ち（D-1）ができるまでは「要確認テーブルに行を作って終える」 | |
| D-8 | PR #902（FEAT-2 の組み込み・draft）を先にマージするか | 先にマージし、本キャンペーンはその上に載せる | |
| D-9 | 実 LLM の検証に使うモデルと枠（Gemini API の Gemma 4 無料枠・有償キー） | | |
| D-10 | AI 作成の閉ループ: 試運転ツール（本人の権限・承認カードつき）、テーブル・保存ビュー・ミニアプリを作るツール、エディタ内 AI、アプリ作成時の手数上限 | どれも入れる。順序は説明文の修正 → 評価ハーネス → 試運転 → 作成ツール | |
| D-11 | テンプレートの記法を `{name}`（ir.md §3.2）のままにするか、`{{name}}` も差し込みとして受けるか | `{name}` のまま。`{{name}}` は保存時に警告し、UI でワンクリック変換 | |
| D-12 | 正本の追記の提案: ir.md §6.2 にファイル保存イベントのペイロード、§7 に csv.* の行、engine.md にパラメータ解決の節、ir.md §8 の V2・V4・V7 の実装状況 | | |
| D-13 | web に単体テストの仕組み（vitest）を足すか（CI の変更を伴う） | 足す（reducer と座標計算を無検査のままにしない） | |
| D-14 | キャンペーンの利用者シナリオを `web/e2e/campaign-*.manual.spec.ts` として残すか | 残す（回帰の網にする） | |
| D-15 | エディタの操作: ノード移動を Undo の対象にするか、クリックのたびのズームを残すか、複数選択・コピー／貼り付けを範囲に入れるか | 移動も Undo 対象。ズームはダブルクリック時だけ | |
| D-16 | ミニアプリ: 下書きと公開を分けるか、束ねた skill の扱い（会話の導線を作るか外すか）、書き損じた app ブロックをエラーにするか、共同編集者の部品権限 | | |
| D-17 | csv.* で xlsx と Shift_JIS を受けるか | | |
| D-18 | `csv.patch` の base_rev を省略可（実行時点の版）にするか | 出力に version を載せるだけにし、必須のまま | |
| D-19 | Yui app に足す部品（ファイル添付・ユーザー／部署・テーブルの行の選択）をどちらのリポジトリで先行させるか | | |
| D-20 | PR の切り方（不具合ごとに 1 PR か、層ごとにまとめるか） | 不具合ごと（BUG-1 はエンジン・AI 説明文・UI の 3 本に分ける） | |

## 9. 検証環境 runbook

このマシン（2 CPU・10GB RAM）では、native 起動（`.claude/skills/pr/scripts/dev-up.sh` の既定）で回す。compose 版（Docker 内で release ビルド）と RAG・sandbox・office は初回ラウンドの対象から外す。

### 9.1 用意するもの

- `target/debug/shiki-server`（`cargo build --bin shiki-server`・sccache が効くので 7 分程度）
- 隔離 DuckDB ランナー: `cargo build --locked --manifest-path crates/tabular/runner/Cargo.toml`（workspace 外。`--locked` を付けないと `crates/tabular/runner/Cargo.lock` が書き換わる）
- compose のインフラ: `cd deploy/compose && cp .env.example .env && docker compose up -d postgres keycloak openfga redis rustfs`
- web: `cd web && pnpm install --frozen-lockfile && pnpm gen:api`

### 9.2 起動

`dev-up.sh` の抜け（BUG-22）は、呼び出し側の export で回避する（親の env は `run-server.sh` に継がれる）。

```bash
export SHIKI__DATA__EPHEMERAL_CURSOR_KEY=true
export SHIKI__TABULAR__RUNNER_PATH=$PWD/crates/tabular/runner/target/debug/shiki-tabular-runner
export SHIKI_CONFIG=~/.config/shiki-dev/llm.toml   # 実 LLM を足すとき（下記）
bash .claude/skills/pr/scripts/dev-up.sh
```

- LLM は既定を stub にし、`SHIKI_CONFIG` の TOML で Gemma 4（Gemini の OpenAI 互換口）を追加のプロバイダとして並べる。stub と実 LLM を同じサーバで両立できる。stub の通常応答は `回答: <user_text>` なので、`llm.invoke` の prompt の解決をそのまま観測できる。
- Gemma は `[[llm.models]] id="gemma-4-31b-it" reasoning_in_content=true` が要る（無いと思考だけで止まる）。Gemini の無料枠は Flash 系が 20 回/日で、モデル間で共有している。
- ログインは `/login` から入る。入力は `#username` / `#password` / `#kc-login`。dev seed は alice（admin）・bob・charlie（別テナント）、パスワードはすべて `password`。

### 9.3 使う・確かめる

- 利用者の操作は Playwright で行う（`web/e2e/campaign-*.manual.spec.ts`・`test.skip(!process.env.CAMPAIGN)`）。
- 裏取りは BFF の `/api/*`（CSRF はヘッダ `x-csrf-token` ＝ Cookie `shiki_csrf`）と、compose の postgres への SQL で行う（ホストに psql が無いので `docker compose exec postgres psql -U postgres shiki`）。
  - run と step: `workflow_run`・`step_execution(output, error)`・`run_event`
  - テーブルの行: `data_record`（`data` のキーは `f{id}`）
- チャットの UI はツールの結果を描画しない。AI 作成の検証では、保存されたワークフロー・run の出力で裏取りする。

### 9.4 既知の罠

- dev seed の alice は旧形式の Keycloak group だと multi テナンシーで 403 になる（Keycloak の DB を流用したとき）。
- secrets が dev/compose とも配線されていない。`http.request` のシークレット参照を試すときは `SHIKI__SECRETS__MASTER_KEY_FILE`（32 バイト）と `SHIKI__WORKFLOW__HTTP_ALLOWLIST` を足す。
- `smoke-bff.sh` は API=:8080・REDIRECT_URI=:8080 が前提で、dev-up の構成（:3000）では使えない。
- チャットの生成が 429 で再試行されると、適用済みの文書編集がもう一度走る（#889）。

## 10. 作業ログ

### 2026-10-25

- 調査（9 本の並列調査）: BUG-1〜5 の原因を特定。BUG-6〜25・GAP-06〜41・APP-01〜12・FEAT-1 の設計案・検証環境の runbook をまとめた。
- 検証環境: `shiki-server` と tabular runner の debug ビルド、web の依存、compose のインフライメージを用意した。
