# Phase 1 — ストレージ

> 目的: Google Drive 風のファイル/フォルダ操作を、**単一 StorageService（権限＋監査＋イベントのチョークポイント）**
> として確立する。これが RAG（Phase 2）・サンドボックスFUSE（Phase 4）・チャットfileツールの共通土台になる。
> 完了の定義(DoD): 権限付きでファイル/フォルダをアップロード・閲覧・移動・共有でき、全操作が監査ログに残り、
> 書込時にイベントが発行される（Phase 2 がそれを購読する）。
>
> ⚠️ **着手前に [設計上の落とし穴](../design-caveats.md) の PIT-6（presigned のチョークポイント漏れ）・
> PIT-11（OpenFGA 整合性）・PIT-12（監査の改竄耐性主張）・PIT-13（権限フィルタ済みページング）・
> PIT-14（dedup 側チャネル）・PIT-16（closure 同時 move）を確認すること。**

## タスク一覧

| ID | タイトル | area | fable5 | 依存 |
|----|---------|------|--------|------|
| 1.1 | ストレージのデータモデル（メタ/ツリー） | storage | – | 0.5 |
| 1.2 | `ObjectStore` トレイト＋MinIO実装＋コンテンツアドレッシング | storage | – | 0.2 |
| 1.3 | StorageService 権限チェック層（OpenFGA） | storage | – | 1.1, 1.2, 0.5 |
| 1.4 | ファイルCRUD API（アップロード/DL/移動/リネーム/削除） | storage | – | 1.3 |
| 1.5 | フォルダ操作＋階層（closure table） | storage | – | 1.1, 1.4 |
| 1.6 | 共有（ReBAC relation: viewer/commenter/editor） | storage | – | 1.3 |
| 1.7 | バージョニング | storage | – | 1.2, 1.4 |
| 1.8 | 書込イベント発行（後段RAGトリガ） | storage | – | 1.4 |
| 1.9 | 監査ログ基盤 | storage | – | 1.3 |
| 1.10 | Drive風UI（ブラウズ/アップロード/共有ダイアログ） | frontend | – | 1.4, 1.5, 1.6 |

---

## 詳細

### Task 1.1: ストレージのデータモデル（メタ/ツリー）
- **area**: storage
- **依存**: 0.5
- **path**: `crates/storage`, migrations
- **仕様**:
  - Postgres スキーマ: `node(id, org_id, parent_id, name, type[file|folder], blob_hash, size, mime,
    current_version, owner_id, created_at, updated_at, deleted_at)`。
  - 階層は **closure table**（`node_closure(ancestor, descendant, depth)`）で移動/継承クエリを高速化。
  - 全アクセスは `AuthContext`（org込み）経由。論理削除（ゴミ箱）に対応。
- **受け入れ条件**:
  - [ ] migration が適用され、ノード作成/取得/移動が closure 整合性を保つ
  - [ ] 同一フォルダ内の名前一意制約
  - [ ] org スコープが全クエリに効く

### Task 1.2: `ObjectStore` トレイト＋MinIO実装＋コンテンツアドレッシング
- **area**: storage
- **依存**: 0.2
- **path**: `crates/storage`
- **仕様**:
  - `ObjectStore` トレイト（put/get/delete/presigned-url）。実装は MinIO(S3)。**GCS実装はPhase 8**。
  - **コンテンツアドレッシング**: blob は内容ハッシュ（例 sha256）をキーに保存→自動重複排除。
    node メタは `blob_hash` を参照。大容量はストリーミング/マルチパート。
  - バケットは内部専用、**直アクセス禁止**（必ず StorageService 経由）。
- **受け入れ条件**:
  - [ ] 同一内容の2ファイルが1 blob を共有する
  - [ ] 大容量ファイルがストリーミングで put/get できる
  - [ ] presigned URL は StorageService の権限判定を経た発行のみ

### Task 1.3: StorageService 権限チェック層（OpenFGA）
- **area**: storage
- **依存**: 1.1, 1.2, 0.5
- **path**: `crates/storage`, `crates/authz`
- **仕様**:
  - `folder`/`file` 型と relations（`owner`/`viewer`/`commenter`/`editor`/`parent`）を OpenFGA model に追加。
  - StorageService の **全 read/write 経路で OpenFGA check を必須化**。フォルダ→子への継承を relation で表現。
  - check 失敗は403、結果は監査ログ（1.9）へ。
- **受け入れ条件**:
  - [ ] viewer でないユーザーの read が403
  - [ ] 親フォルダ viewer が子を継承して read できる
  - [ ] StorageService を介さない経路が存在しない（レビュー＋テスト）

### Task 1.4: ファイルCRUD API
- **area**: storage
- **依存**: 1.3
- **path**: `crates/api`, `crates/storage`
- **仕様**:
  - REST: アップロード（multipart/stream）、ダウンロード、メタ取得、リネーム、移動、削除（論理）、復元。
  - OpenAPI注釈→型生成。各操作で権限check＋監査＋（書込は）イベント発行。
- **受け入れ条件**:
  - [ ] 各操作がE2Eで動作し権限が効く
  - [ ] 移動で closure が更新される
  - [ ] 削除→ゴミ箱→復元が機能する

### Task 1.5: フォルダ操作＋階層
- **area**: storage
- **依存**: 1.1, 1.4
- **path**: `crates/storage`, `crates/api`
- **仕様**:
  - フォルダ作成/移動/削除、子一覧（ページング）、パンくず（祖先列）取得。closure を用いた配下一括取得。
  - フォルダ移動時の権限継承の再評価（OpenFGAは relation 追従なので tuple 整合のみ確認）。
- **受け入れ条件**:
  - [ ] 深い階層の移動が closure 整合を保つ
  - [ ] 子一覧が権限フィルタ済み（読めるものだけ）
  - [ ] 循環移動を拒否

### Task 1.6: 共有（ReBAC relation）
- **area**: storage
- **依存**: 1.3
- **path**: `crates/storage`, `crates/authz`, `crates/api`
- **仕様**:
  - ファイル/フォルダを user / department / group に対して viewer/commenter/editor で共有/解除するAPI。
  - OpenFGA tuple の付与/削除として実装。共有相手一覧・自分が共有された一覧の取得。
- **受け入れ条件**:
  - [ ] 部署共有で部署メンバ全員が継承アクセスできる
  - [ ] 共有解除で即時にアクセス不可
  - [ ] 個別例外（フォルダ共有でも特定ファイル除外）が表現できる

### Task 1.7: バージョニング
- **area**: storage
- **依存**: 1.2, 1.4
- **path**: `crates/storage`
- **仕様**:
  - ファイル更新ごとに新 version（blob_hash, author, timestamp）を記録。履歴一覧/特定版取得/復元。
  - コンテンツアドレッシングにより同一内容の版はblob共有。エージェントの破壊的編集の安全網。
- **受け入れ条件**:
  - [ ] 更新で版が増え、過去版をDL/復元できる
  - [ ] 復元が新しい版として記録される（履歴を壊さない）

### Task 1.8: 書込イベント発行
- **area**: storage
- **依存**: 1.4
- **path**: `crates/storage`
- **仕様**:
  - create/update/delete/move 時に**ドメインイベント**（node_id, version, op, org, actor）を発行。
  - **初版キュー = Postgres ベース（pgmq 等）**。購読側（Phase 2 ingestion）が増分再索引に使う。
    outbox パターンでトランザクション整合を担保。
- **受け入れ条件**:
  - [ ] 書込と同一トランザクションでイベントが outbox に入る
  - [ ] 購読側がat-least-once で受信できる
  - [ ] FUSE経由の書込（Phase 4）も同経路に乗る設計

### Task 1.9: 監査ログ基盤
- **area**: storage
- **依存**: 1.3
- **path**: `crates/storage`, `crates/authz`
- **仕様**:
  - 全データ操作と認可判定（who/what/object/decision/trace_id）を構造化記録。
  - trace_id を OTel と共有し、後で Langfuse とも突合できる土台（Phase 3/設計の核）。
  - 監査ログは改竄耐性を意識（append-only テーブル）。
- **受け入れ条件**:
  - [ ] read/write/share/deny が全て記録される
  - [ ] trace_id でトレースと突合できる
  - [ ] 監査ログの参照に管理権限が要る

### Task 1.10: Drive風UI
- **area**: frontend
- **依存**: 1.4, 1.5, 1.6
- **path**: `web/`
- **仕様**:
  - フォルダブラウズ（パンくず/一覧/並べ替え）、ドラッグ&ドロップアップロード、移動/リネーム/削除、
    共有ダイアログ（相手検索＝user/dept、権限選択）、版履歴表示。
  - 生成済み型付きクライアントを使用。読めるものだけ表示（サーバ側フィルタ前提）。
- **受け入れ条件**:
  - [ ] ファイル/フォルダのCRUDがUIから完結
  - [ ] 共有ダイアログで部署/個人に権限付与できる
  - [ ] 版履歴から復元できる
