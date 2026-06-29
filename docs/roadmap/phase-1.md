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

| ID | タイトル | area | 依存 |
|----|---------|------|------|
| 1.1 | ストレージのデータモデル（メタ/ツリー） | storage | 0.5 |
| 1.2 | `ObjectStore` トレイト＋MinIO実装＋コンテンツアドレッシング | storage | 0.2 |
| 1.3 | StorageService 権限チェック層（OpenFGA） | storage | 1.1, 1.2, 0.5 |
| 1.4 | ファイルCRUD API（アップロード/DL/移動/リネーム/削除） | storage | 1.3 |
| 1.5 | フォルダ操作＋階層（closure table） | storage | 1.1, 1.4 |
| 1.6 | 共有（ReBAC relation: viewer/commenter/editor） | storage | 1.3 |
| 1.7 | バージョニング | storage | 1.2, 1.4 |
| 1.8 | 書込イベント発行（後段RAGトリガ） | storage | 1.4 |
| 1.9 | 監査ログ基盤 | storage | 1.3 |
| 1.10 | Drive風UI（ブラウズ/アップロード/共有ダイアログ） | frontend | 1.4, 1.5, 1.6 |

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
  - **PIT-6 の決定（presigned 採用）**: バイト転送は **presigned URL 方式**（クライアント↔MinIO 直）。
    ただし単一チョークポイントを守るため**メタ・認可・監査・content-addressing は必ず StorageService 経由**。
    アップロードは**二相**（declare→presigned PUT(staging)→finalize で server-side 再ハッシュ検証→content-addressed へ昇格）、
    DL は**短 TTL の presigned GET**（発行時に viewer check＋監査）。発行後 TTL 満了までは失効しない残存ウィンドウと、
    実バイト GET がアプリ監査経路外である点は正直に明記（capability 発行を監査の正とする）。署名は公開エンドポイントで行う。
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
- **決定（実装済み）**:
  - move は「移動サブツリー ∪ 移動先の祖先列」を id 昇順ロックした単一 txn で closure を張り替える（PIT-16）。
    FGA は移動ノードの parent タプルのみ差し替え、子は `from parent` 継承で追従。
  - 子一覧は **オーバーフェッチ＋keyset カーソル**（`(name, id)` 昇順）で読めない子を読み飛ばす（PIT-13）。
    末尾でちょうど埋まった場合に空ページが 1 回返り得るが、欠落・重複は起きない。
  - 循環は「移動先が自身の closure 配下」をロック下で判定して 400 で拒否。
- **受け入れ条件**:
  - [x] 深い階層の移動が closure 整合を保つ
  - [x] 子一覧が権限フィルタ済み（読めるものだけ）
  - [x] 循環移動を拒否

### Task 1.6: 共有（ReBAC relation）
- **area**: storage
- **依存**: 1.3
- **path**: `crates/storage`, `crates/authz`, `crates/api`
- **仕様**:
  - ファイル/フォルダを user / role / group に対して viewer/commenter/editor で共有/解除するAPI。
  - OpenFGA tuple の付与/削除として実装。共有相手一覧・自分が共有された一覧の取得。
- **決定（実装済み・human 合意）**: design.md §4.1 のストレージ ReBAC 図に合わせ、共有 relation は **viewer/editor のみ**
  （commenter は thread 専用＝Phase 3。files のコメント機能実装時に再検討）。共有先は **user のみ**。
  共有の付与/解除/一覧管理は **owner 権限**（editor の再共有による権限横展開＝confused-deputy を防ぐ）。
  剥奪の即時反映は **read 認可の HIGHER_CONSISTENCY**（PIT-11）。書込/管理系の check は MINIMIZE_LATENCY。
- **defer（#76 へ）**: **role 共有**は OpenFGA の `role` 型が tenant 無スコープ（識別子の tenant スコープ化＝SAAS.1
  未実装）かつ role provisioning（SAAS.2）未実装のため defer。現状は user 共有のみ ship し、越境は DB の
  `org+tenant` フィルタが backstop。**group** 共有・**個別例外**（`but not blocked`）も同 issue で扱う。
- **受け入れ条件**:
  - [x] user 共有で対象ユーザーがアクセスでき、非対象には漏れない
  - [x] 共有解除で即時にアクセス不可
  - [ ] role 共有でメンバ全員が継承アクセス → **#76 へ defer（SAAS.1/SAAS.2 前提）**
  - [ ] 個別例外（フォルダ共有でも特定ファイル除外）→ **#76 へ defer**

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
    - この**キュー/outbox 基盤はインジェスト専用ではなく汎用**で、Phase 3 のチャット生成ジョブ（Task 3.11・接続非依存生成）も同じ pgmq＋outbox に乗る。
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
  - [ ] 共有ダイアログでロール/個人に権限付与できる
  - [ ] 版履歴から復元できる
