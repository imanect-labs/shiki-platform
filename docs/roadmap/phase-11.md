# Phase 11 — エディタ／Office 統合（Yjs 共同編集・Collabora）

> 目的: 文書作成・共同編集を製品の第一級市民にする。md 系は **Yjs/yrs（CRDT）＋TipTap**、
> Office 系（docx/pptx/xlsx）は **Collabora Online**（共同編集は内蔵に委任・自作しない）。
> **AI は md では共同編集参加者としてリアルタイム編集、Office では非セッション時のファイルレベル編集**
> という非対称を正直な仕様とする（design §4.8/§4.8.1）。旧 V2 トラック（parallel-tracks）は本フェーズに統合。
> 完了の定義(DoD): 複数ユーザーが md 文書を同時編集でき、AI が同一セッションに参加して編集/サジェストできる。
> Collabora で Office 文書を共同編集でき、保存が WOPI→StorageService→バージョニング→RAG 再索引に流れる。
> スプレッドシートのカスタム関数/マクロが shiki script で動く。
>
> ⚠️ **着手前に [設計上の落とし穴](../design-caveats.md) の PIT-37（Yjs update log の肥大化と権限）・
> PIT-11（WOPI トークンと共有解除の即時性）を確認すること。**

| ID | タイトル | area | 依存 |
|----|---------|------|------|
| 11.1 | Yjs 同期サーバ（yrs・axum WebSocket・update log/snapshot 永続化） | api | 1.x |
| 11.2 | md ドキュメント種（真実=Yjs・保存時 md シリアライズ→StorageService） | storage | 11.1 |
| 11.3 | TipTap WYSIWYG エディタ（y-prosemirror・awareness・権限反映） | frontend | 11.2 |
| 11.4 | AI 共同編集クライアント（document.edit ツール・サジェスト/直接適用） | ai | 11.2, 3.3 |
| 11.5 | OfficeSuite トレイト＋Collabora デプロイ（SaaS 共有プール/オンプレ同梱） | docgen | – |
| 11.6 | WOPI ホスト（StorageService クライアント・毎呼び出し ReBAC・バージョニング） | docgen | 11.5, 1.x |
| 11.7 | Office 共同編集＋保存→RAG 再索引（旧 V2.2〜V2.4 統合） | docgen | 11.6, 2.x |
| 11.8 | AI Office 編集（非セッション時ファイルレベル・セッション中は提案保存） | ai | 11.6, 7.x |
| 11.9 | スプレッドシート×shiki script（カスタム関数/マクロ） | data | 11.7, 10.7 |
| 11.10 | ドライブ更新者表示・バージョン履歴 UI（updated_by） | frontend | 1.x |

---

## 詳細

### Task 11.1: Yjs 同期サーバ
- **area**: api / **path**: `crates/collab`
- **仕様**: yrs で WebSocket 同期を axum 内に実装（新規ステートフル依存ゼロ）。update log＋定期 snapshot を
  Postgres/StorageService へ（tenant_id スコープ）。ドキュメント参加は editor/viewer relation を接続時＋定期に再チェック。
- **受け入れ条件**:
  - [ ] 2クライアントの並行編集が収束する（オフライン→再接続含む）
  - [ ] viewer は読めるが書けない・共有解除で接続が切断される
  - [ ] update log が snapshot に圧縮され無限肥大しない

### Task 11.2: md ドキュメント種
- **area**: storage / **path**: `crates/collab`, `crates/storage`
- **仕様**: 真実は Yjs ドキュメント。保存時に正規化 md へシリアライズし StorageService に書く
  （→書込イベント→RAG 再索引が既存経路で動く）。md 側の直接編集（エージェントの file write 等）は
  「インポート」として Yjs 側に取り込む単方向規約。
- **受け入れ条件**:
  - [ ] 編集内容が md として保存され、検索（RAG）に反映される
  - [ ] 表・埋め込みブロックがシリアライズ往復で壊れない
  - [ ] ファイル側の外部書込が編集セッションと衝突せず取り込まれる

### Task 11.3: TipTap エディタ
- **area**: frontend / **path**: `web/`
- **仕様**: TipTap＋y-prosemirror。Obsidian/Notion 風 WYSIWYG。awareness（カーソル・参加者表示）。
  ReBAC（viewer/commenter/editor）を UI モードに反映。
- **受け入れ条件**:
  - [ ] 見出し/表/コード/チェックリスト/埋め込みが編集できる
  - [ ] 参加者のカーソルとプレゼンスが表示される

### Task 11.4: AI 共同編集クライアント
- **area**: ai / **path**: `crates/collab`, `crates/agent-core`
- **仕様**: エージェントの `document.edit` ツールは Yjs トランザクションを発行する専用クライアントとして
  セッション参加（awareness に「AI」表示）。サジェスト（提案マーク）/直接適用の2モード。
  権限は実行主体の editor relation（人間と同一経路）。ファイル直接上書きの経路を作らない。
- **受け入れ条件**:
  - [ ] 人間の編集中に AI が同時編集しても収束し、変更が AI 名義で表示される
  - [ ] editor 権限のない実行主体の AI 編集が拒否される
  - [ ] サジェストモードで人間が承認/棄却できる

### Task 11.5: OfficeSuite トレイト＋Collabora デプロイ
- **area**: docgen / **path**: `crates/office`, `deploy/`
- **仕様**: `OfficeSuite` トレイト（OnlyOffice への差し替え退路）。Collabora(CODE) コンテナを
  SaaS=テナント共有プール／オンプレ・エアギャップ=同梱で配備。
- **受け入れ条件**:
  - [ ] compose/k8s に Collabora が入り docx/pptx/xlsx が開ける
  - [ ] エアギャップ構成で外部接続なしに動く

### Task 11.6: WOPI ホスト
- **area**: docgen / **path**: `crates/office`
- **仕様**: CheckFileInfo/GetFile/PutFile を **StorageService の一クライアント**として実装（チョークポイント維持・
  直バケット禁止）。WOPI access_token=（実行主体×ファイル×短寿命）＋**毎呼び出し ReBAC 再チェック**
  （HIGHER_CONSISTENCY・共有解除の即時反映）。トークンにテナント境界を焼き込む。PutFile→新バージョン→書込イベント。
- **受け入れ条件**:
  - [ ] 共有解除後の既存編集セッションが次の WOPI 呼び出しで拒否される
  - [ ] PutFile が監査・バージョニング・RAG 再索引に乗る
  - [ ] 他テナントのファイルにトークンを流用できない

### Task 11.7: Office 共同編集＋RAG 還流
- **area**: docgen / **path**: `crates/office`, `web/`
- **仕様**: iframe 組込・複数ユーザー同時編集（Collabora 内蔵）。保存→再索引の整合（旧チャンク失効）。
- **受け入れ条件**:
  - [ ] 2ユーザーが同一 pptx を同時編集できる
  - [ ] 保存後の検索が新内容を反映し旧バージョンのチャンクが混入しない

### Task 11.8: AI Office 編集
- **area**: ai / **path**: `crates/office`, `ingestion-worker/`
- **仕様**: read=Docling パースを正（convert-to は補助）。edit=WOPI ロックで判定し、
  非セッション時のみファイルレベル編集（ingestion-worker の生成系を編集に拡張）→新バージョン。
  セッション中は「提案として保存」（別バージョン/コメント）。ライブ参加はスコープ外（将来）。
- **受け入れ条件**:
  - [ ] AI が pptx/docx/xlsx を読み・編集し新バージョンとして保存できる
  - [ ] 編集セッション中の AI 編集要求が上書きせず提案保存に落ちる

### Task 11.9: スプレッドシート×shiki script
- **area**: data / **path**: `crates/office`, `crates/script-runtime`
- **仕様**: シートのカスタム関数/マクロを shiki script（10.7 ランタイム再利用・ms 級起動）で実行。
  実行主体=操作ユーザー（対話トリガと同じ交差則）。
- **受け入れ条件**:
  - [ ] セル関数として script が評価され再計算に耐えるレイテンシで返る
  - [ ] script がユーザーの読めないデータに到達できない

### Task 11.10: ドライブ更新者表示・バージョン履歴 UI
- **area**: frontend / **path**: `web/`, `crates/storage`
- **仕様**: `updated_by` をノードメタ＋バージョン作者に保持し、一覧・詳細・バージョン履歴（誰が・いつ・どの版）に表示。
- **受け入れ条件**:
  - [ ] ファイル一覧と詳細に最終更新者が出る
  - [ ] バージョン履歴から過去版の作者を辿れる
