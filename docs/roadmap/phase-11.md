# Phase 11 — スライド（自前）＋ Office 統合（Collabora）

> 目的: **スライドを自前の第一級ドキュメント**（GrapesJS＋Yjs＋pptx エクスポート・design §4.8.3）として提供し、
> Office 互換ファイル（docx/xlsx/既存 pptx）のブラウザ内編集・共同編集を **Collabora Online**
> （ソース自前ビルド・共同編集は内蔵に委任）で提供する。
> **「人間編集中は AI 編集不可」は撤廃**: ネイティブ3種（ノート/スライド/CSV）は AI が共同編集エンジンに
> 常時参加。Collabora 文書のみ、セッション中の AI 編集は提案バージョン保存に落とす（design §4.8）。
> md 系（ノート）は [Phase 11-pre](./phase-11-pre.md) で完了済み。
>
> 完了の定義(DoD): チャット「パワポを作成して」→スライド下書き→保存→複数ユーザー＋AI の同時編集→
> pptx エクスポート→Collabora で再編集、が一続きで動く。「表を作成して」→CSV 下書き→保存が動く。
> 3エディタで選択→AI 指示ができる。Collabora で Office 文書を共同編集でき、保存が WOPI→StorageService→
> バージョニング→RAG 再索引に流れる。
> **スコープ外**: 11.9（スプレッドシート×shiki script）は本フェーズ完遂の対象外（将来イシュー）。
> Collabora セッションへの AI ライブ参加は**ポストアルファから前倒しで実装済み**
> （#328 → issue #352・`office.live_edit`／AI が CoolWSD の headless 参加者・design §4.8）。
> ⚠️ **着手前に [設計上の落とし穴](../design-caveats.md) の PIT-11（WOPI トークンと共有解除の即時性）・
> PIT-40〜44（スライド XSS/並行編集/pptx 忠実度/Collabora サプライチェーン/提案保存）を確認すること。**
>
> **実装突合（2026-10-17・main 329aba0）**: 11.1〜11.11（11.9 除く）は全タスクで実装と本番配線まで確認できた。
> ただし DoD の主経路（スライドエディタ・pptx エクスポート・AI スライド編集・Office 全般）の e2e が CI で常時 skip で、
> 仕様の一部（要素単位差し替え・変換可能性 lint・チャートのネイティブ変換・下書き画面の選択→AI・提案採用時の衝突再確認）が未実装。
> 残り: スライド系 e2e が CI で常時 skip（CI イメージにエディタ砂箱が無い）#706／Office 系 e2e が CI で走らない（OFFICE_E2E 未設定）#707／
> pptx 変換判定が粗い（丸ごとラスタ化・svg 消失・チャート非ネイティブ）#708／`ReplaceElement` 未実装 #709／
> 変換可能性 lint とレイアウト閉集合が無い #710／提案採用時に最新版との衝突を再確認しない（PIT-44 ③）#711／
> 下書き画面から選択→AI ができない #712／選択→AI の入力検証不足（draft_name 未検証・区切りエスケープなし・negative テストなし）#713／
> Collabora 自前ビルドの再現性（週次クリーンビルド未検証・digest 未固定・可変タグ）#714／
> SaaS 共有プール・エアギャップ構成が compose 以外に無い #715／砂箱ブリッジの targetOrigin が `"*"` #716／
> AI 編集の名義が UI に出ない #695／同一スライド並行編集の well-formed IT が無い #704／
> Docx の内容をコピーできない #489／要素粒度 CRDT（ポストアルファ）#318／11.9 据え置き #317。

| ID | タイトル | area | 依存 |
|----|---------|------|------|
| 11.1 | スライド doc 種（DocKind 一般化・`.slide`・sanitize/serialize/saver・RAG）＋閲覧ビュー | storage/frontend | 11P.1, 11P.2 |
| 11.2 | GrapesJS 砂箱エディタ＋Yjs 共同編集（別オリジン＋通信全遮断 CSP 配信・MessagePort ブリッジ。旧: opaque origin 配信 → 置換: design §4.8.3） | frontend | 11.1, 9.x(B1) |
| 11.3 | AI スライド編集（slide.read/slide.edit）＋save_slide 下書き＋テンプレート/テーマ | ai | 11.2, 11P.4, 11P.5 |
| 11.4 | pptx エクスポート（DOM 計測→pptxgenjs ネイティブ変換＋要素単位ラスタライズ） | frontend | 11.2 |
| 11.5 | OfficeSuite トレイト＋Collabora ソース自前ビルド/デプロイ（オンプレ同梱・SaaS 共有プール） | docgen | – |
| 11.6 | WOPI ホスト（StorageService クライアント・毎呼び出し ReBAC・バージョニング） | docgen | 11.5, 1.x |
| 11.7 | Office 共同編集＋保存→RAG 再索引（旧 V2.2〜V2.4 統合） | docgen | 11.6, 2.x |
| 11.8 | AI Office 編集（非ロック時ファイルレベル・ロック中は提案バージョン） | ai | 11.6, 7.x |
| 11.9 | スプレッドシート×shiki script（カスタム関数/マクロ）**（スコープ外・据え置き）** | data | 11.7, 10.7 |
| 11.10 | 選択→AI 指示（ノート/CSV/スライド共通の SelectionContext） | frontend/ai | 11.2, 11P.3, 11P.8 |
| 11.11 | csv_draft（「表を作成して」→CSV 下書き画面・save_csv） | ai/frontend | 11P.8, 11P.5 |

---

## 詳細

### Task 11.1: スライド doc 種＋閲覧ビュー
- **area**: storage/frontend / **path**: `crates/collab`, `crates/api`, `web/`, `ingestion-worker/`
- **仕様**: `crates/collab` の doc 種判定（`.md` 拡張子分岐）を `DocKind` 閉集合へ一般化し、
  `.slide`（MIME `application/vnd.shiki.slide+json`）を追加。真実=Yjs（`Map "meta"`（ノートと共用）＋`Array "slides"`、
  各スライド=`Map {id, html: Y.Text, notes: Y.Text, bg}`）。保存時に正規化 JSON へシリアライズ→
  `update_file_content_internal`（版/監査/outbox/RAG 既存経路）。**書込全経路で ammonia サニタイズ**
  （PIT-40 の第1層）。parse.py に `.slide` ハンドラ（スライド順 HTML 連結→既存 html パス）。
  閲覧は `/slides/{id}` ページ＋ srcdoc `sandbox=""` ビューア（DOMPurify 通過後のみ注入）。
  ドライブ open 分岐・新規作成「スライド」を実装（ダミートースト撤去）。
- **受け入れ条件**:
  - [x] `.slide` の Yjs 編集が保存で新バージョンになり、RAG 検索にスライド本文が乗る
    （`crates/collab/tests/slide_it.rs`・`ingestion-worker/tests/test_parse.py`）
  - [x] serialize 往復（JSON⇄Yjs）が壊れない・敵対的 HTML がシリアライズ出力に残らない（adversarial テスト CI）
    （`crates/collab/src/slide/{mod,model,sanitize}.rs` の単体テスト）
  - [x] script 入り `.slide` を直接アップロードしても、どの経路でも実行されない（e2e negative）
    （`web/e2e/slides.spec.ts`・CI で実走）

### Task 11.2: GrapesJS 砂箱エディタ＋Yjs 共同編集
- **area**: frontend / **path**: `web/editor-sandbox/`, `crates/app-gateway`, `web/`
- **仕様**: GrapesJS core（BSD-3）＋ブリッジを self-contained バンドルにビルドし、app-gateway 第3リスナ
  （apps オリジン＝アプリ本体と別オリジン）の `/builtin/` から配信。隔離は「**別オリジン＋通信全遮断 CSP**
  （`default-src 'none'`・`builtin_csp`）」— GrapesJS はキャンバス iframe へ同一オリジンで触る必要があり
  **opaque origin にはしない**（design §4.8.3 の確定判断。ユーザー供給の B1 バンドルは従来どおり opaque origin）。
  親が Yjs doc/CollabProvider を保持し MessagePort でスライド HTML の入出力のみ（zod 検証・PIT-23 同型）。
  エコー抑制は origin タグ＋デバウンス＋diff。viewer は読み取り専用。
  （旧: opaque origin で配信 → 置換: 別オリジン＋通信全遮断 CSP・iframe は `sandbox="allow-scripts allow-same-origin"`・
  根拠 design §4.8.3／`crates/app-gateway/src/builtin.rs` の `builtin_csp`）
  実装注記: 砂箱ブリッジは MessageChannel の port 移譲時に targetOrigin `"*"` のまま（apps オリジンへ固定する → #716）。
  同一スライド並行編集後の HTML が well-formed であることの IT（PIT-41 受け入れ条件）は無い → #704。
- **受け入れ条件**:
  - [ ] 2ユーザーの同時編集が収束する（e2e 2コンテキスト）（実装済み・検証テスト欠落 #706。
    `web/e2e/slides-editor.spec.ts` はエディタ砂箱が CI イメージに無く常時 skip。サーバ側の収束は
    `crates/api/tests/collab_ws_it.rs` で検証済み）
  - [x] エディタが別オリジン＋通信全遮断であることが CSP golden テストで固定される（builtin_csp）
  - [x] viewer 権限では編集 UI が無効・書込が届かない（書込破棄は `collab_ws_it.rs` の `viewer_reads_but_cannot_write`）

### Task 11.3: AI スライド編集＋下書き＋テンプレート
- **area**: ai / **path**: `crates/collab`, `crates/chat`, `crates/agent-core`, `web/`
- **仕様**: `SlideEditOp`（AppendSlide/InsertSlideAfter/ReplaceSlide/RemoveSlide/SetNotes/ReplaceElement/
  SetMeta/SetBackground）を `apply_ai_slide_edit`（既存 `apply_ai_edit` 同型・editor relation・
  HigherConsistency）で適用。vocab に `slide.read`/`slide.edit`（要確認）/`save_slide`（下書き・確認不要）。
  `save_slide` は note_drafts と同型の**下書き確定型**（slide_drafts→カード→`/slides/draft`→「ドライブに保存」）。
  テーマカタログ＋レイアウトパターンを閉集合で持ちプロンプトへ焼き込み。変換可能性 lint の警告を EditReport で返す。
  実装注記: `SlideEditOp` に `ReplaceElement` は無い（AI はスライド単位の `ReplaceSlide` のみ → #709）。
  テーマはカタログ 4 種（`crates/chat/src/slide_templates.rs`）で閉集合化済みだが、レイアウトパターンの閉集合と
  変換可能性 lint は未実装（EditReport は `applied`/`skipped` のみ → #710）。
- **受け入れ条件**:
  - [ ] 人間の編集中に AI が同時編集しても収束し、AI 名義で表示される（排他なし）→ #695
    （収束は `crates/collab/tests/slide_ai_edit_it.rs` で検証済み。AI 名義は update log の author `ai:<user>` のみで UI 表示が無い）
  - [x] editor 権限のない実行主体の slide.edit が拒否される（`slide_ai_edit_it.rs` の `ai_slide_edit_denied_without_editor`）
  - [x] 「パワポを作成して」→下書き画面→保存→`/slides/{id}` が一続きで動く（e2e）
    （`web/e2e/slide-conversation-drafts.spec.ts`・CI で実走）

### Task 11.4: pptx エクスポート
- **area**: frontend / **path**: `web/editor-sandbox/`, `web/`
- **仕様**: 砂箱内で DOM 計測（1280×720・テーマ同梱フォント）→ pptxgenjs でテキスト/画像/図形/背景/表/
  チャートをネイティブシェイプへ変換。変換不能要素のみ**要素単位**で画像化（全体ラスタライズ禁止・PIT-42）。
  変換レポートを保存ダイアログに表示。bytes は親へ返し既存アップロード API で `.pptx` 保存。
  実装注記: ネイティブ変換はテキスト/リスト/表/画像/図形ボックス/背景まで（`web/editor-sandbox/src/export.ts`）。
  チャートのネイティブ変換は無く、変換不能判定（`needsRaster`）は子孫に SVG や背景画像を持つコンテナを丸ごと画像化し、
  トップレベルの `<svg>` は変換もレポートもされずに落ちる → #708。フォントは `fontFace: "Yu Gothic"` 固定で、
  テーマ同梱フォントでの計測は未実装。
- **受け入れ条件**:
  - [ ] テンプレート由来スライドがネイティブシェイプとして PowerPoint/Collabora で再編集できる
    （実装済み・検証テスト欠落 #706・#707。テンプレート由来スライドと Collabora での再編集を検証するテストが無い）
  - [ ] 変換不能要素だけが画像化され、レポートに件数が出る → #708（判定が粗く、件数が 1 以上になる場合のテストも無い）
  - [ ] e2e で .pptx が生成され `ppt/slides/slide1.xml` にテキストが存在する（実装済み・検証テスト欠落 #706。
    `web/e2e/slides-export.spec.ts` はエディタ砂箱が CI イメージに無く常時 skip）

### Task 11.5: OfficeSuite トレイト＋Collabora ソース自前ビルド/デプロイ
- **area**: docgen / **path**: `crates/office`, `deploy/`
- **仕様**: `OfficeSuite` トレイト（OnlyOffice への差し替え退路・discovery キャッシュ・fail-closed）。
  **配布物は MPLv2 ソースからの自前ビルド**（`deploy/docker/collabora/`・タグ pin＋sha256 manifest・
  fork-policy 準拠・PIT-43）。ビルドは別 CI ワークフロー（タグ/週次/manifest 変更トリガ）でレジストリ push、
  開発/CI は暫定 CODE pin 可。compose は `profiles: ["office"]`・127.0.0.1 バインド。
  SaaS=テナント共有プール／オンプレ・エアギャップ=同梱（実行時 DL なし）。
  実装注記: 自前ビルドは `deploy/docker/collabora/`（`office-manifest.env` でタグ＋コミット SHA の二重ピン・Poco は sha256）
  と `.github/workflows/office-image.yml`（smoke→GHCR push）。#502 の修正後の週次クリーンビルドは未実行で、
  ベースイメージ `ubuntu:24.04` は digest 未固定、compose は毎週上書きされるタグ `cp-25.04.9-5` を参照する → #714。
  SaaS 共有プール・オンプレのエアギャップ同梱物の構成は compose 以外に無い → #715。
- **受け入れ条件**:
  - [ ] compose profile office で docx/pptx/xlsx が開ける（実装済み・検証テスト欠落 #707。
    `web/e2e/office.spec.ts` は docx のみで、`OFFICE_E2E=1` のときだけ実行）
  - [ ] エアギャップ構成で外部接続なしに動く → #715
  - [ ] 自前ビルドイメージに接続数/文書数の人工制限がない（実装済み・検証テスト欠落 #715。
    根拠は `deploy/docker/collabora/README.md` の記載（configure 既定の 9999 接続・9999 文書）のみ）

### Task 11.6: WOPI ホスト
- **area**: docgen / **path**: `crates/office`
- **仕様**: CheckFileInfo/GetFile/PutFile/Lock 系を **StorageService の一クライアント**として実装
  （チョークポイント維持・直バケット禁止）。WOPI access_token=（実行主体×ファイル×短寿命・HMAC）＋
  **毎呼び出し ReBAC 再チェック**（HIGHER_CONSISTENCY・共有解除の即時反映）。トークンにテナント境界を焼き込む。
  PutFile→`update_file_content_internal`→新バージョン→書込イベント。`office_lock`（WOPI 準拠 30 分 TTL・lazy 解放）。
- **受け入れ条件**:
  - [x] 共有解除後の既存編集セッションが次の WOPI 呼び出しで拒否される
    （`crates/office/tests/wopi_it.rs` の `revocation_takes_effect_on_next_call`）
  - [x] PutFile が監査・バージョニング・RAG 再索引に乗る（`wopi_it.rs` の `checkfileinfo_getfile_putfile_happy_path`）
  - [x] 他テナントのファイルにトークンを流用できない（`wopi_it.rs` の `tenant_boundary_is_enforced`）

### Task 11.7: Office 共同編集＋RAG 還流
- **area**: docgen / **path**: `crates/office`, `web/`
- **仕様**: `/office/{id}` iframe 組込（form post でトークン注入・PostMessageOrigin）・複数ユーザー同時編集
  （Collabora 内蔵）。ドライブ open 分岐に docx/xlsx/pptx を追加（Collabora 未配備時はダウンロードへ
  フォールバック）。「ドキュメント」新規作成=同梱最小 .docx テンプレ。保存→再索引の整合（旧チャンク失効）。
  実装注記: Docx の内容をコピーできない不具合が未解決 → #489。
- **受け入れ条件**:
  - [ ] 2ユーザーが同一 pptx を同時編集できる（Collabora 内蔵機能に委任・実装済み・検証テスト欠落 #707）
  - [ ] 保存後の検索が新内容を反映し旧バージョンのチャンクが混入しない（実装済み・検証テスト欠落 #707。
    旧版チャンクの掃除は `crates/rag/tests/fulltext_it.rs`・`vector_qdrant_it.rs` で単体検証済みだが、Office 保存からの通しテストが無い）

### Task 11.8: AI Office 編集
- **area**: ai / **path**: `crates/office`, `ingestion-worker/`, `crates/storage`
- **仕様**: read=Docling パースを正。edit=WOPI ロックで判定し、非ロック時はファイルレベル編集
  （ingestion-worker `edit.py`・python-docx/openpyxl/python-pptx・ステートレス bytes 入出力）→新バージョン。
  **ロック中は「提案バージョン」**（`node_version.is_proposal`・current を進めない・RAG 索引除外・
  履歴 UI で editor が採用→通常新バージョン化・PIT-44）。ツール=`doc_read`（索引済み本文の読み取り）/`office.edit`（要確認）
  （旧: `office.read` を新設 → 置換: 新設せず Docling 索引を読む `doc_read`・根拠 #425／`crates/chat/src/office_tool.rs`）。
  実装注記: 提案の採用（`adopt_proposal_version`）は最新版との衝突を再確認せず、提案のベース版も記録しない（PIT-44 ③ 未達 → #711）。
  Collabora へのライブ参加は本タスクのスコープ外だったが、issue #352 で実装済み
  （`office.live_edit`＝AI が CoolWSD セッションの headless 参加者としてアンカー指定編集。
  旧: ポストアルファ → 置換: 前倒し実装・根拠 #352／design §4.8）。
  **新規作成**（`save_document` / `save_sheet`）は issue #381 で Collabora へ一本化した
  （空テンプレ実体化→HTML/セル paste。md 下書き画面 `/office/draft` は廃止・design §4.8・PIT-50。
  旧: md 下書き画面で作成 → 置換: Collabora 一本化・根拠 #381）。
  作成/編集の結果は `document_ref` カードとしてチャットに残る（#358 の実害を解消）。
- **受け入れ条件**:
  - [x] AI が pptx/docx/xlsx を読み・編集し新バージョンとして保存できる
    （`crates/office/tests/edit_it.rs` の `unlocked_edit_creates_new_version_and_emits_event`・`ingestion-worker/tests/test_edit.py`）
  - [x] 編集セッション中の AI 編集要求が上書きせず提案保存に落ちる（negative テスト）
    （`edit_it.rs` の `locked_edit_becomes_proposal_then_adopt_promotes`）
  - [x] 提案の採用で通常バージョンに昇格し書込イベントが流れる（同上。採用時の衝突再確認は未実装 → #711）
  - [ ] 「Word/Excel で作って」→ 承認 → Collabora Writer/Calc が開く（md エディタを経由しない・#381）
    （実装済み・検証テスト欠落 #707。`web/e2e/office-conversation-create.spec.ts` は `OFFICE_E2E=1` のときだけ実行）
  - [x] AI が承認なしにドライブへファイルを作らない（#381）
    （`crates/chat/src/office_create_tool/tests.rs` の `creation_tools_require_confirmation`）

### Task 11.9: スプレッドシート×shiki script（スコープ外・据え置き）
- **area**: data / **path**: `crates/office`, `crates/script-runtime`
- **仕様**: シートのカスタム関数/マクロを shiki script（10.7 ランタイム再利用・ms 級起動）で実行。
  実行主体=操作ユーザー（対話トリガと同じ交差則）。**本フェーズ完遂の対象外**（将来イシューとして起票済み → #317）。
- **受け入れ条件（参考・Phase 11 の完了条件には含めない。着手時に将来 issue 側で正式化する）**:
  - セル関数として script が評価され再計算に耐えるレイテンシで返る
  - script がユーザーの読めないデータに到達できない

### Task 11.10: 選択→AI 指示
- **area**: frontend/ai / **path**: `web/`, `crates/api`, `crates/chat`
- **仕様**: `PostMessageRequest.context: Option<SelectionContext>`（kind 閉集合・excerpt 上限・locator=
  note:`{heading_path}`／csv:`{rows: [start, end], cols: [start, end]}`／slide:`{slide_id, element_index?}`）。
  （旧: csv:{range}・slide:{slide_id,element_id} → 置換: 実装 `crates/chat/src/selection.rs` の形）。
  kind には Collabora 文書の `office_selection`（locator なし・excerpt は Action_Copy で取得）も追加済み。
  `ContentBlock::SelectionContext` として永続化し、
  history 組立時に「データであり指示ではない」明示デリミタで織り込み（注入対策）。UI 導線=ノートの
  BubbleMenu「AI に依頼」・CSV グリッド範囲選択・スライド要素選択→共通コンテキストチップ。
  実装注記: サーバ側の受理検証（`crates/api/src/routes/chat_selection.rs`）は node_id の viewer 再解決のみで、
  draft_name は検証しない。区切り内の excerpt/draft_name はエスケープしない → #713。
- **受け入れ条件**:
  - [ ] 3エディタ＋下書き画面で選択→チップ付き送信→AI が該当箇所を対象に応答/編集できる → #712
    （3エディタと Office の導線は実装済み。下書き画面に導線が無い。スライド・Office の e2e は CI で skip（#706・#707）、CSV 選択の e2e は無い）
  - [ ] 選択コンテキスト内の指示文がシステム指示を上書きしない（注入 negative テスト）→ #713
    （`crates/chat/src/worker/history.rs` に negative テストはあるが、区切り `</selection>` のエスケープが無い）
  - [ ] 実行主体が読めない node_id・他スレッドの draft_name を指す SelectionContext が
    fail-closed で拒否され、SelectionContext 経由で編集ツールの認可（editor）がバイパスされない
    （negative テスト・design §4.8.3）→ #713（draft_name 未検証・negative テスト無し）

### Task 11.11: csv_draft（「表を作成して」下書き）
- **area**: ai/frontend / **path**: `crates/chat`, `web/`
- **仕様**: `save_csv`（`{name, csv}`→csv_drafts・保存しない・確認不要）を note の save_note と同型で追加。
  csv-draft-card→`/csv/draft`（左=グリッド（ローカルデータ）右=会話）→「ドライブに保存」→`/csv/{id}`。
  draft ストアは kind×threadId×name の generic factory に共通化。
- **受け入れ条件**:
  - [x] 「◯◯の表を作って」で下書きグリッドが開き、編集後ドライブ保存→CSV エディタへ遷移
    （`web/e2e/csv-conversation-drafts.spec.ts`・CI で実走。下書きグリッドでのセル編集操作自体は e2e で未操作）
  - [x] 下書きがリロード/別端末でスレッド履歴から復元される（同 spec の localStorage 消去→リロード復元）
