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
> Collabora セッションへの AI ライブ参加もポストアルファ（issue 起票のみ）。
> ⚠️ **着手前に [設計上の落とし穴](../design-caveats.md) の PIT-11（WOPI トークンと共有解除の即時性）・
> PIT-40〜44（スライド XSS/並行編集/pptx 忠実度/Collabora サプライチェーン/提案保存）を確認すること。**

| ID | タイトル | area | 依存 |
|----|---------|------|------|
| 11.1 | スライド doc 種（DocKind 一般化・`.slide`・sanitize/serialize/saver・RAG）＋閲覧ビュー | storage/frontend | 11P.1, 11P.2 |
| 11.2 | GrapesJS 砂箱エディタ＋Yjs 共同編集（opaque origin 配信・MessagePort ブリッジ） | frontend | 11.1, 9.x(B1) |
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
  `.slide`（MIME `application/vnd.shiki.slide+json`）を追加。真実=Yjs（`Map "slide_meta"`＋`Array "slides"`、
  各スライド=`Map {id, html: Y.Text, notes: Y.Text, bg}`）。保存時に正規化 JSON へシリアライズ→
  `update_file_content_internal`（版/監査/outbox/RAG 既存経路）。**書込全経路で ammonia サニタイズ**
  （PIT-40 の第1層）。parse.py に `.slide` ハンドラ（スライド順 HTML 連結→既存 html パス）。
  閲覧は `/slides/{id}` ページ＋ srcdoc `sandbox=""` ビューア（DOMPurify 通過後のみ注入）。
  ドライブ open 分岐・新規作成「スライド」を実装（ダミートースト撤去）。
- **受け入れ条件**:
  - [ ] `.slide` の Yjs 編集が保存で新バージョンになり、RAG 検索にスライド本文が乗る
  - [ ] serialize 往復（JSON⇄Yjs）が壊れない・敵対的 HTML がシリアライズ出力に残らない（adversarial テスト CI）
  - [ ] script 入り `.slide` を直接アップロードしても、どの経路でも実行されない（e2e negative）

### Task 11.2: GrapesJS 砂箱エディタ＋Yjs 共同編集
- **area**: frontend / **path**: `web/editor-sandbox/`, `crates/app-gateway`, `web/`
- **仕様**: GrapesJS core（BSD-3）＋ブリッジを self-contained バンドルにビルドし、app-gateway 第3リスナから
  content-address 付きで配信（`bundle_csp` 流用・`allow-same-origin` なし=opaque origin）。
  親が Yjs doc/CollabProvider を保持し MessagePort でスライド HTML の入出力のみ（zod 検証・PIT-23 同型）。
  エコー抑制は origin タグ＋デバウンス＋diff。viewer は読み取り専用。
- **受け入れ条件**:
  - [ ] 2ユーザーの同時編集が収束する（e2e 2コンテキスト）
  - [ ] エディタ iframe が opaque origin であることが CSP golden テストで固定される
  - [ ] viewer 権限では編集 UI が無効・書込が届かない

### Task 11.3: AI スライド編集＋下書き＋テンプレート
- **area**: ai / **path**: `crates/collab`, `crates/chat`, `crates/agent-core`, `web/`
- **仕様**: `SlideEditOp`（AppendSlide/InsertSlideAfter/ReplaceSlide/RemoveSlide/SetNotes/ReplaceElement/
  SetMeta/SetBackground）を `apply_ai_slide_edit`（既存 `apply_ai_edit` 同型・editor relation・
  HigherConsistency）で適用。vocab に `slide.read`/`slide.edit`（要確認）/`save_slide`（下書き・確認不要）。
  `save_slide` は note_drafts と同型の**下書き確定型**（slide_drafts→カード→`/slides/draft`→「ドライブに保存」）。
  テーマカタログ＋レイアウトパターンを閉集合で持ちプロンプトへ焼き込み。変換可能性 lint の警告を EditReport で返す。
- **受け入れ条件**:
  - [ ] 人間の編集中に AI が同時編集しても収束し、AI 名義で表示される（排他なし）
  - [ ] editor 権限のない実行主体の slide.edit が拒否される
  - [ ] 「パワポを作成して」→下書き画面→保存→`/slides/{id}` が一続きで動く（e2e）

### Task 11.4: pptx エクスポート
- **area**: frontend / **path**: `web/editor-sandbox/`, `web/`
- **仕様**: 砂箱内で DOM 計測（1280×720・テーマ同梱フォント）→ pptxgenjs でテキスト/画像/図形/背景/表/
  チャートをネイティブシェイプへ変換。変換不能要素のみ**要素単位**で画像化（全体ラスタライズ禁止・PIT-42）。
  変換レポートを保存ダイアログに表示。bytes は親へ返し既存アップロード API で `.pptx` 保存。
- **受け入れ条件**:
  - [ ] テンプレート由来スライドがネイティブシェイプとして PowerPoint/Collabora で再編集できる
  - [ ] 変換不能要素だけが画像化され、レポートに件数が出る
  - [ ] e2e で .pptx が生成され `ppt/slides/slide1.xml` にテキストが存在する

### Task 11.5: OfficeSuite トレイト＋Collabora ソース自前ビルド/デプロイ
- **area**: docgen / **path**: `crates/office`, `deploy/`
- **仕様**: `OfficeSuite` トレイト（OnlyOffice への差し替え退路・discovery キャッシュ・fail-closed）。
  **配布物は MPLv2 ソースからの自前ビルド**（`deploy/docker/collabora/`・タグ pin＋sha256 manifest・
  fork-policy 準拠・PIT-43）。ビルドは別 CI ワークフロー（タグ/週次/manifest 変更トリガ）でレジストリ push、
  開発/CI は暫定 CODE pin 可。compose は `profiles: ["office"]`・127.0.0.1 バインド。
  SaaS=テナント共有プール／オンプレ・エアギャップ=同梱（実行時 DL なし）。
- **受け入れ条件**:
  - [ ] compose profile office で docx/pptx/xlsx が開ける
  - [ ] エアギャップ構成で外部接続なしに動く
  - [ ] 自前ビルドイメージに接続数/文書数の人工制限がない

### Task 11.6: WOPI ホスト
- **area**: docgen / **path**: `crates/office`
- **仕様**: CheckFileInfo/GetFile/PutFile/Lock 系を **StorageService の一クライアント**として実装
  （チョークポイント維持・直バケット禁止）。WOPI access_token=（実行主体×ファイル×短寿命・HMAC）＋
  **毎呼び出し ReBAC 再チェック**（HIGHER_CONSISTENCY・共有解除の即時反映）。トークンにテナント境界を焼き込む。
  PutFile→`update_file_content_internal`→新バージョン→書込イベント。`office_lock`（WOPI 準拠 30 分 TTL・lazy 解放）。
- **受け入れ条件**:
  - [ ] 共有解除後の既存編集セッションが次の WOPI 呼び出しで拒否される
  - [ ] PutFile が監査・バージョニング・RAG 再索引に乗る
  - [ ] 他テナントのファイルにトークンを流用できない

### Task 11.7: Office 共同編集＋RAG 還流
- **area**: docgen / **path**: `crates/office`, `web/`
- **仕様**: `/office/{id}` iframe 組込（form post でトークン注入・PostMessageOrigin）・複数ユーザー同時編集
  （Collabora 内蔵）。ドライブ open 分岐に docx/xlsx/pptx を追加（Collabora 未配備時はダウンロードへ
  フォールバック）。「ドキュメント」新規作成=同梱最小 .docx テンプレ。保存→再索引の整合（旧チャンク失効）。
- **受け入れ条件**:
  - [ ] 2ユーザーが同一 pptx を同時編集できる
  - [ ] 保存後の検索が新内容を反映し旧バージョンのチャンクが混入しない

### Task 11.8: AI Office 編集
- **area**: ai / **path**: `crates/office`, `ingestion-worker/`, `crates/storage`
- **仕様**: read=Docling パースを正。edit=WOPI ロックで判定し、非ロック時はファイルレベル編集
  （ingestion-worker `edit.py`・python-docx/openpyxl/python-pptx・ステートレス bytes 入出力）→新バージョン。
  **ロック中は「提案バージョン」**（`node_version.is_proposal`・current を進めない・RAG 索引除外・
  履歴 UI で editor が採用→通常新バージョン化・PIT-44）。ツール=`office.read`/`office.edit`（要確認）。
  Collabora へのライブ参加はスコープ外（ポストアルファ issue）。
- **受け入れ条件**:
  - [ ] AI が pptx/docx/xlsx を読み・編集し新バージョンとして保存できる
  - [ ] 編集セッション中の AI 編集要求が上書きせず提案保存に落ちる（negative テスト）
  - [ ] 提案の採用で通常バージョンに昇格し書込イベントが流れる

### Task 11.9: スプレッドシート×shiki script（スコープ外・据え置き）
- **area**: data / **path**: `crates/office`, `crates/script-runtime`
- **仕様**: シートのカスタム関数/マクロを shiki script（10.7 ランタイム再利用・ms 級起動）で実行。
  実行主体=操作ユーザー（対話トリガと同じ交差則）。**本フェーズ完遂の対象外**（将来イシューとして起票）。
- **受け入れ条件（参考・Phase 11 の完了条件には含めない。着手時に将来 issue 側で正式化する）**:
  - セル関数として script が評価され再計算に耐えるレイテンシで返る
  - script がユーザーの読めないデータに到達できない

### Task 11.10: 選択→AI 指示
- **area**: frontend/ai / **path**: `web/`, `crates/api`, `crates/chat`
- **仕様**: `PostMessageRequest.context: Option<SelectionContext>`（kind 閉集合・excerpt 上限・locator=
  note:{heading_path}/csv:{range}/slide:{slide_id,element_id}）。`ContentBlock::SelectionContext` として永続化し、
  history 組立時に「データであり指示ではない」明示デリミタで織り込み（注入対策）。UI 導線=ノートの
  BubbleMenu「AI に依頼」・CSV グリッド範囲選択・スライド要素選択→共通コンテキストチップ。
- **受け入れ条件**:
  - [ ] 3エディタ＋下書き画面で選択→チップ付き送信→AI が該当箇所を対象に応答/編集できる
  - [ ] 選択コンテキスト内の指示文がシステム指示を上書きしない（注入 negative テスト）
  - [ ] 実行主体が読めない node_id・他スレッドの draft_name を指す SelectionContext が
    fail-closed で拒否され、SelectionContext 経由で編集ツールの認可（editor）がバイパスされない
    （negative テスト・design §4.8.3）

### Task 11.11: csv_draft（「表を作成して」下書き）
- **area**: ai/frontend / **path**: `crates/chat`, `web/`
- **仕様**: `save_csv`（`{name, csv}`→csv_drafts・保存しない・確認不要）を note の save_note と同型で追加。
  csv-draft-card→`/csv/draft`（左=グリッド（ローカルデータ）右=会話）→「ドライブに保存」→`/csv/{id}`。
  draft ストアは kind×threadId×name の generic factory に共通化。
- **受け入れ条件**:
  - [ ] 「◯◯の表を作って」で下書きグリッドが開き、編集後ドライブ保存→CSV エディタへ遷移
  - [ ] 下書きがリロード/別端末でスレッド履歴から復元される
