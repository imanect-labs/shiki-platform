# Phase 11-pre — ノート（md）／CSV エディタ

> 目的: Phase 11（Office/Collabora）に先行して、**ノート（Notion/Loop 風 md 共同編集）**と
> **CSV エディタ（グリッド編集＋読み取り専用 SQL 分析）**を製品の第一級市民にする。
> md 系の基盤設計（真実=Yjs・TipTap・AI は共同編集参加者）は design §4.8.1、CSV は design §4.8.2 が正本。
> 旧 Phase 11 の md 系タスク（11.1〜11.4）と 11.10 は本フェーズへ移動した（Office 系は phase-11.md に残置）。
>
> 完了の定義(DoD): 「新規作成 > ノート」から作成した md 文書を複数ユーザーが同時編集でき、
> ノートに紐づくチャットパネルから AI が同一 Yjs セッションで編集できる。保存は md シリアライズ→
> StorageService→RAG 再索引に流れる。CSV はグリッドで無限スクロール編集・RO SQL 分析ができ、
> 同じ能力（csv.query/patch/write）をチャットのエージェントとワークフローが使える。
>
> ⚠️ **着手前に [設計上の落とし穴](../design-caveats.md) の PIT-37（Yjs update log の肥大化と権限・
> md に落ちない情報）・PIT-39（DuckDB の外部アクセス遮断とリソース隔離）を確認すること。**

| ID | タイトル | area | 依存 |
|----|---------|------|------|
| 11P.1 | Yjs 同期サーバ（yrs・axum WebSocket・update log/snapshot 永続化） | api | 1.x |
| 11P.2 | md ドキュメント種「ノート」（真実=Yjs・保存時 md シリアライズ・frontmatter メタデータ） | storage | 11P.1 |
| 11P.3 | TipTap WYSIWYG エディタ（スラッシュコマンド・メタデータパネル・awareness・権限反映） | frontend | 11P.2 |
| 11P.4 | AI 共同編集クライアント（document.edit・直接適用既定／サジェスト切替） | ai | 11P.2, 3.3 |
| 11P.5 | ノートページ×チャット分割ビュー（note_ref カード・thread_id 紐付け・新規作成「ノート」） | frontend | 11P.3 |
| 11P.6 | 埋め込みブロック（genui シキコンポーネント／ミニアプリ・artifact iframe／ドライブ参照） | frontend | 11P.3, 6.x, 9.x |
| 11P.7 | CSV クエリサービス（`crates/tabular`・隔離 DuckDB・RO SQL・ページング） | data | 1.x |
| 11P.8 | CSV グリッドエディタ（無限スクロール・パッチ編集・楽観ロック・SQL コンソール） | frontend | 11P.7 |
| 11P.9 | CSV エージェント／ワークフロー公開（csv.query / csv.patch / csv.write） | ai | 11P.7, 10.x |
| 11P.10 | ドライブ更新者表示・バージョン履歴 UI（updated_by・旧 11.10） | frontend | 1.x |

**スコープ外（将来イシューとして起票のみ）**: ①JSX サンドボックス実行ブロック（ノート内ミニアプリ＝B1 相当の
ビルド/配布が必要）②BI（複数 CSV へのクエリ層＋チャート・ダッシュボード。11P.7 のクエリサービスが土台）
③Notion 型プロパティ DB（型付きプロパティ・テーブルビュー。Phase 9 data_table との統合設計が必要）。

---

## 詳細

### Task 11P.1: Yjs 同期サーバ
- **area**: api / **path**: `crates/collab`
- **仕様**: yrs で WebSocket 同期を axum 内に実装（新規ステートフル依存ゼロ）。update log＋定期 snapshot を
  Postgres/StorageService へ（tenant_id スコープ）。ドキュメント参加は editor/viewer relation を接続時＋定期に再チェック。
- **受け入れ条件**:
  - [ ] 2クライアントの並行編集が収束する（オフライン→再接続含む）
  - [ ] viewer は読めるが書けない・共有解除で接続が切断される
  - [ ] update log が snapshot に圧縮され無限肥大しない

### Task 11P.2: md ドキュメント種「ノート」
- **area**: storage / **path**: `crates/collab`, `crates/storage`
- **仕様**: 真実は Yjs ドキュメント。保存時に正規化 md へシリアライズし StorageService に書く
  （→書込イベント→RAG 再索引が既存経路で動く）。md 側の直接編集（エージェントの file write 等）は
  「インポート」として Yjs 側に取り込む単方向規約。メタデータは **frontmatter 型の軽量属性**
  （タイトル・アイコン・タグ・任意 key-value・紐付く thread_id）を Yjs 内に保持し、シリアライズ時に
  YAML frontmatter へ落とす（往復可能・RAG も拾える）。型検証・集計・フィルタはやらない（将来の Notion 型 DB は別トラック）。
- **受け入れ条件**:
  - [ ] 編集内容が md として保存され、検索（RAG）に反映される
  - [ ] 表・埋め込みブロック・frontmatter がシリアライズ往復で壊れない
  - [ ] ファイル側の外部書込が編集セッションと衝突せず取り込まれる

### Task 11P.3: TipTap エディタ
- **area**: frontend / **path**: `web/`
- **仕様**: TipTap＋y-prosemirror。Obsidian/Notion/Loop 風 WYSIWYG。awareness（カーソル・参加者表示）。
  ReBAC（viewer/commenter/editor）を UI モードに反映。**スラッシュコマンド**（見出し/表/コード/チェックリスト/
  埋め込み/AI アクション）。ノート上部に**メタデータ（プロパティ）パネル**（11P.2 の frontmatter 属性の閲覧・編集）。
- **受け入れ条件**:
  - [ ] 見出し/表/コード/チェックリスト/埋め込みがスラッシュコマンドから挿入・編集できる
  - [ ] 参加者のカーソルとプレゼンスが表示される
  - [ ] メタデータパネルでタグ・任意 key-value を編集でき frontmatter に反映される

### Task 11P.4: AI 共同編集クライアント
- **area**: ai / **path**: `crates/collab`, `crates/agent-core`
- **仕様**: エージェントの `document.edit` ツールは Yjs トランザクションを発行する専用クライアントとして
  セッション参加（awareness に「AI」表示）。**既定は直接適用**（AI 名義・Yjs undo 可）、サジェスト
  （提案マーク→承認/棄却）はトグルまたは明示依頼で切替。権限は実行主体の editor relation（人間と同一経路）。
  ファイル直接上書きの経路を作らない。
- **受け入れ条件**:
  - [ ] 人間の編集中に AI が同時編集しても収束し、変更が AI 名義で表示される
  - [ ] editor 権限のない実行主体の AI 編集が拒否される
  - [ ] サジェストモードで人間が承認/棄却できる

### Task 11P.5: ノートページ×チャット分割ビュー
- **area**: frontend / **path**: `web/`, `crates/chat`
- **仕様**: **ノートページが分割ビューをホスト**し、既存チャットスレッド UI（conversation コンポーネント）を
  開閉式サイドパネルとして再利用（分割ビューの実装は一箇所のみ・二重ホストしない）。ノートは紐付く thread_id を
  メタデータに保持。チャット側からは **note_ref カード**（workflow_ref と同型）で「ノートとして保存/開く」→
  ノートページへ遷移（チャットパネル開状態）。どちらのパネルも折りたたみ可。ドライブ「新規作成」に「ノート」を追加。
  ノートの共有とスレッドの共有は別 ReBAC — パネルはスレッド閲覧権限がない共同編集者に対して fail-closed で
  「アクセスなし」表示に落とす（スレッドを暗黙共有しない）。
- **受け入れ条件**:
  - [ ] 新規作成「ノート」→編集→チャットパネルで AI に編集依頼、が一続きで動く
  - [ ] チャットの note_ref カードから AI 生成 md をノートとして保存し開ける
  - [ ] スレッド閲覧権限のないノート共同編集者にスレッド内容が漏れない

### Task 11P.6: 埋め込みブロック
- **area**: frontend / **path**: `web/`
- **仕様**: 埋め込みは次の 3 種**のみ**: ①genui の検証済みシキコンポーネントスペック（Phase 6 レンダラ再利用）
  ②ミニアプリ／artifact の別オリジン iframe（Phase 9 B1 と同じオリジン分離・CSP）③ドライブファイル参照
  （画像プレビュー等・閲覧者の ReBAC で解決）。**生 HTML/JSX はレンダリングしない**（コードブロック表示のみ）—
  共同編集文書での stored XSS を遮断するため、既存の信頼境界（検証済みスペック or 別オリジン）以外を作らない。
  JSX のサンドボックス実行は将来イシュー。
- **受け入れ条件**:
  - [ ] 3 種の埋め込みが挿入・表示でき、md シリアライズ往復で壊れない
  - [ ] script を含む生 HTML を貼っても実行されない（表示のみ）
  - [ ] ドライブ参照埋め込みは閲覧者本人の権限で解決される（作成者の権限を借用しない）

### Task 11P.7: CSV クエリサービス
- **area**: data / **path**: `crates/tabular`
- **仕様**: CSV は **StorageService 上のファイルが真実**（authz はファイル単位 ReBAC・data_table には乗せない）。
  `crates/tabular` を CSV クエリ/パッチの**単一チョークポイント**とし、DuckDB 実行は**非特権別プロセスに隔離**
  （sandbox-wasm/script-runtime と同じ隔離パターン・敵対的 CSV を api プロセスに食わせない）。
  SQL は**読み取り専用**（DDL/DML 拒否）・対象は AuthContext で読めるファイルのみ・**外部アクセス無効化**
  （`enable_external_access=false`・httpfs 等の extension 無効。PIT-39）。メモリ/時間/結果サイズのクォータを強制。
  結果はページ配信（グリッドの無限スクロールと同じページング API を共用）。
- **受け入れ条件**:
  - [ ] 読めないファイルへのクエリ・`read_csv('/etc/...')` 等の外部参照・DML が全て拒否される（fail-closed）
  - [ ] クォータ超過クエリが api を巻き込まず隔離プロセス内で打ち切られる
  - [ ] 10 万行級 CSV でページ取得がインタラクティブなレイテンシで返る

### Task 11P.8: CSV グリッドエディタ
- **area**: frontend / **path**: `web/`
- **仕様**: グリッド UI（仮想化＋ページ取得で無限スクロール）。編集はセル/行/列の**パッチ操作**
  （cell_update/row_insert/row_delete/column_add 等）を rev 付きで送り、**楽観ロック**（rev 不一致は拒否→リロード）
  →新バージョン保存（既存バージョニング・書込イベントに乗る）。CRDT 共同編集はやらない。
  **SQL コンソール**（RO・11P.7 経由）を併設し、結果を「新規 CSV として保存」は明示操作として提供。
  ドライブ「新規作成 > その他」に「CSV」を追加。既存 .csv ファイルも同エディタで開ける。
- **受け入れ条件**:
  - [ ] 10 万行級 CSV をスクロール・編集でき、全量ダウンロードが発生しない
  - [ ] 並行編集の衝突が rev で検出され、黙って上書きされない
  - [ ] SQL 結果の閲覧と「新規 CSV として保存」ができる

### Task 11P.9: CSV エージェント／ワークフロー公開
- **area**: ai / **path**: `crates/agent-core`, `crates/workflow-engine`, `crates/tabular`
- **仕様**: `csv.query`（RO SQL・ページング結果）／`csv.patch`（パッチ操作→新バージョン）／`csv.write`
  （新規 CSV 保存）を、チャットのエージェントツールとワークフローステップの**両方**に公開。
  すべて 11P.7 の同一チョークポイントを通り、実行主体のファイル ReBAC で判定（Phase 10 の実行主体交差則と同じ）。
- **受け入れ条件**:
  - [ ] チャットで「この CSV を分析して」が csv.query 経由で動き、引用元が監査に残る
  - [ ] ワークフローから行追記→新バージョン→書込イベントが流れる
  - [ ] 実行主体が読めないファイルへのツール呼び出しが拒否される

### Task 11P.10: ドライブ更新者表示・バージョン履歴 UI
- **area**: frontend / **path**: `web/`, `crates/storage`
- **仕様**: `updated_by` をノードメタ＋バージョン作者に保持し、一覧・詳細・バージョン履歴（誰が・いつ・どの版）に表示。
  （旧 Phase 11 Task 11.10 の前倒し。ノート/CSV の編集・AI 編集で「誰が更新したか」が即座に必要になるため）
- **受け入れ条件**:
  - [ ] ファイル一覧と詳細に最終更新者が出る（AI 編集は AI 名義）
  - [ ] バージョン履歴から過去版の作者を辿れる
