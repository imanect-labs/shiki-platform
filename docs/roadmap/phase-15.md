# Phase 15 — 構造化データ基盤 v2（Teable / Microsoft Lists 級）

> 目的: Phase 9 で立ち上げた `crates/data` を、**業務アプリ基盤として通用するテーブル基盤**へ引き上げ、
> **workflow / shiki script / generative UI / chat の全面から同一のクエリ IR とビュー定義で参照できる**ようにする。
> **設計正本は [data-platform.md](../data-platform.md)**（本書は実装順のみを定める）。
>
> 完了の定義(DoD): ユーザーが日本語の列名を持つテーブルを作り、条件木＋複数ソートのグリッドを
> 無限スクロールで閲覧・編集でき、他ユーザーの編集がリアルタイムに反映される。
> テーブル間のリンク・ロールアップ・数式が動き、参照先の行ポリシーが透過適用される。
> 同じテーブルを、ワークフローの `data.*` ノード・shiki script の `Shiki.data.*`・
> generative UI の束縛・チャットのツールから、**いずれも呼び出しユーザーの権限で**操作できる。
> そして**プール全体で 10⁴〜10⁶ テーブルに達しても、索引の物理本数が増えない**。

## 位置づけと緊急度

**15.1〜15.3（再基盤化）はプライベートアルファ前の必須項目**である。理由は 2 つ。

1. **現行の索引方式はフルプールで破綻する。** v1 は索引宣言フィールドごとに `data_record` 上へ
   partial 式インデックスを張るため、索引本数が**テナント数 × テーブル数 × 索引列数**で増え、
   それが単一 relation にぶら下がる。PostgreSQL は 1 行の INSERT で全索引を open し partial 述語を
   全件評価するため、**顧客 20〜50 社で劣化し数百社で停止する**（data-platform.md §1.1）。
   SaaS のデータプレーンはフルプール（design §4.1・SAAS.5 達成済み）なので、これはリリースブロッカー。
2. **データが小さい今が移行の最安時機。** 15.1（JSONB キー書換）と 15.2（索引バックフィル）は
   レコード数に比例する。顧客が入ってからでは移行コストが跳ね上がる。

15.4 以降（機能拡張・UI・各面公開）はアルファと並走してよい。

## 完了状況（2026-10-24）

**15.0〜15.12 を実装した**（ブランチ `claude/phase15-data-platform-v2`）。受け入れ条件は各タスクの `[x]` に根拠（テスト名か
[data/benchmarks.md](../data/benchmarks.md) の節）を添えた。実装で仕様を変えた点は各タスクに「Phase 15 実装で変更」として記し、
設計正本（[data-platform.md](../data-platform.md)・[query-optimizer.md](../data/query-optimizer.md)）にも反映した。横断的な変更:

- **ステータスコード**: 本書が「422」と書いていた拒否のうち、入力値・スキーマの検証（`DataError::Invalid`）は既存の API 慣例どおり
  **400**、入口の拒否（SQL 前段面・資源上限・ViewSpec の保存時検証＝`DataError::Rejected`）だけが構造化した **422**（data-platform §5.6）
- **索引の本数**はパーティションあたり **13 本**の定数（`data_record` の一意索引と §5.3 の複合索引表を数えに入れた）
- **配信の採番**は consumer ごとではなく**テーブルごとのストリーム**（data-platform §9.2）
- **性能の計測**は全て dev ビルド・同一ホストの PostgreSQL 16 での値（benchmarks.md §0・§5）

## 依存

- **Phase 9**（`crates/data` v1・4 階層 authz・ゲートウェイ能力面）
- **Phase 10**（workflow-engine の IR `Condition` 語彙・ノードカタログ・`effect_journal`）
- **Phase 6**（generative UI の `ActionBinding` / `ActionDispatcher`）
- **Phase 11-pre**（グリッド UI の前例 `web/src/components/csv/csv-grid.tsx`・glide-data-grid）

> ⚠️ **着手前に [設計上の落とし穴](../design-caveats.md) の PIT-69〜76 を確認すること。**
> v1 の PIT-17〜21（集計からの特定・述語材料の量・マスク列での並べ替え・テーブル間の権限素通り・
> WHERE 強制注入の限界）は**そのまま生き続ける**。v2 はこれに加えて、
> pivot 索引のプランナ統計欠如と駆動索引の不在（PIT-69）・SSE の権限材料鮮度と述語グループ化の破綻（PIT-70）・
> 計算列の材料化残留（PIT-71）・書込増幅と肥大化とバックフィル競合（PIT-72）・
> 可視性を失った行の配信漏れ（PIT-73）・**SQL 互換面が authz バイパスの最短経路になり得ること（PIT-74）**・
> 統計が見えない行のサイドチャネルになること（PIT-75）・オプティマイザの書き換えと切替が結果を変え得ること（PIT-76）
> を持ち込む。
>
> **PIT-74 は Task 15.10 の主要な脅威である。** AST を IR に落とさずそのまま SQL テキストへ
> 組み立て直すのが最も自然な近道であり最も危険。`SELECT *` はマスク列を射影へ紛れ込ませ、
> 任意 `JOIN` は PIT-20 の穴を SQL 面から再び開く。エラーメッセージの差分は列の存在オラクルになる。
> 入口の資源上限（`statement_timeout` は DB 実行時間の上限でしかない）も忘れやすい。

| ID | タイトル | area | 依存 |
|----|---------|------|------|
| 15.0 | フルプール正本化（ドキュメント整合）＋ data-platform.md 制定 | infra | – |
| 15.1 | フィールド識別子 3 層移行＋内部整数キーレジストリ＋PoolRouter 継ぎ目 | data | 15.0 |
| 15.2 | pivot 索引テーブル導入＋二重書込＋オンラインバックフィル | data | 15.1 |
| 15.3 | クエリコンパイラ v2（条件木・keyset・戦略選択 O1・EXPLAIN ゲート）＋読取切替＋旧方式撤去 | data | 15.2 |
| 15.4 | リンク／ルックアップ／ロールアップ／数式＋Materialization 規則 | data | 15.3 |
| 15.5 | 型付き ViewSpec ＋ フロント（テーブル一覧・グリッド・スキーマエディタ） | frontend | 15.3 |
| 15.6 | レコード変更 outbox ＋ SSE 差分配信 ＋ 索引の自動昇格 | data | 15.3 |
| 15.7 | 各面公開: workflow data ノード／shiki script／generative UI 束縛／chat ツール | api | 15.3, 15.4 |
| 15.8 | RAG 統合スパイク（構造化データの permission-aware 検索） | rag | 15.3 |
| 15.9 | 性能: 認可材料のリクエストスコープ memo＋BatchCheck＋スキーマキャッシュ | data | 15.3 |
| 15.10 | 性能: SQL 互換インターフェース（parse→IR）＋最適化層＋プランキャッシュ | data | 15.3, **15.4**（リンク JOIN のため） |
| 15.11 | 性能: 一括書込パス（COPY・チャンク Tx・索引遅延構築） | data | 15.2 |
| 15.12 | 性能: 自前クエリオプティマイザ O2/O3（統計収集・コスト較正・頑健な選択・フィードバック・拡張戦略） | data | 15.3, 15.6（O3 は 15.4） |

---

## 詳細

### Task 15.0: フルプール正本化（ドキュメント整合）＋ data-platform.md 制定
- **area**: infra / **path**: `docs/`
- **依存**: –
- **仕様**:
  - **実装（全テーブル `tenant_id`・FGA 識別子名前空間化・Qdrant 単一 collection・プール型 Redis）は当初から
    フルプールだが、requirements/roadmap/phase-0/phase-8/phase-12 に「顧客ごと隔離 cell が正」という
    記述が残り実装と乖離していた**。`parallel-tracks.md` の SAAS.5 は既に全項目達成済みで、
    フルプールは事実上の既定トポロジになっている。これを全ドキュメントで統一する。
  - `docs/data-platform.md` を新規制定し、`crates/data` の設計正本とする（design §4.10 から参照）。
  - フルプールが課す制約 —— **「テナント数に比例して物理オブジェクト（索引・テーブル）が増える設計を禁じる」**
    —— を NFR-8 に明記し、全フェーズへ適用する。
- **受け入れ条件**:
  - [x] 「cell 隔離が正」と読める記述が正本ドキュメントに残っていない（将来オプションとしての言及は可）（#378・取り残しは #826）
  - [x] design §4.1 ⇔ §4.1.1 ⇔ requirements §1.1 ⇔ roadmap の SaaS トポロジ記述が矛盾しない（#378・#826）
  - [x] テナント消去（design §4.12・Phase 12.9）が「ストアごと捨てられない」前提で書き直されている（#378）

### Task 15.1: フィールド識別子 3 層移行＋内部整数キーレジストリ＋PoolRouter 継ぎ目
- **area**: data / **path**: `crates/data/src/model.rs`, `crates/data/src/schema.rs`, `crates/storage/src/tenant.rs`, migrations
- **依存**: 15.0
- **仕様**:
  - `FieldDef` を **`id`（u16・不変・JSONB キー `f{id}`・索引の `field_id`。`0` は owner 擬似列に予約）/
    `key`（`^[a-z][a-z0-9_]{0,63}$`・不変・script/IR/SDK の参照名）/ `display_name`（自由文・**日本語可**・可変）**
    の 3 層に分離する。`order` / `description` / `searchable` も追加（data-platform.md §4）。
  - 内部整数キーを導入: `tenant_int int4`（tenant レジストリで採番）・`table_int int8`（グローバル連番・
    後のパーティションキー）・**`record_int int8`**（プール共通のシーケンス `data_record_int_seq`・`data_record` に列を足し
    `(tenant_int, table_int, record_int)` の一意索引を張る）。**採番は不変・再利用しない**（data-platform.md §3.2）。
  - **`record_int` は外へ出さない**。API・SSE・監査・OpenFGA のタプルは uuid のまま。プール共通の連番は他テナントの
    作成ペースを推測させるため（PIT-75）。keyset カーソルは**サーバ鍵の AEAD で暗号化**した不透明トークンにする。
    個別共有の uuid は材料解決時に 1 本のクエリで `record_int` へ引き直す。
  - 既存データの移行: `name → key`、`display_name = name`、`id` は宣言順に 16 から採番（`0..15` は擬似列予約）、
    JSONB のキーを `f{id}` へ書き換える（テーブル単位・jobq バッチ・中断再開可）。
  - **`tenant → pool` 解決の継ぎ目を単一チョークポイントに入れる**: `DataStore` が `PgPool` を直接持つのをやめ、
    `PoolRouter` から取る形にする。**継ぎ目を後から入れるのは高いので先に入れる**（実分割はしない・
    data-platform.md §12 第 2 段）。あわせて**シャード安全の禁止事項**（テナント横断 JOIN 禁止・
    グローバル連番への依存禁止・横断集計は合算で書く）を文書化し、可能なものは CI で検査する。
- **受け入れ条件**:
  - [x] 日本語の列名（`display_name`）でテーブルを作成・表示・変更でき、変更時に `data_record` が 1 行も更新されない
        （`field_identity_it::japanese_display_name_rename_touches_no_record`）
  - [x] `key` の変更が API で拒否される（不変性の強制）（`field_identity_it::key_change_is_rejected`・400）
  - [x] 既存テーブルの移行がテーブル単位で冪等に実行でき、中断後に再開できる
        （`field_identity_it::legacy_tables_migrate_idempotently_and_v1_operations_still_work`）
  - [x] 移行後も v1 の IT（`data_it.rs` / `policy_threat_it.rs` / `query_view_it.rs` / `fsm_it.rs`）が全て通る（同上・4 ファイルとも現行で通る）
  - [x] `crates/data` 内から `PgPool` を直接参照している箇所が無い（PoolRouter 経由に統一）
        （`shard_safety_lint::data_crate_follows_shard_safety_rules` の規則 S1）
  - [x] 既存レコードに `record_int` が採番され、API 応答・SSE・監査・エラー文言に `record_int` が現れない（negative test）
        （`field_identity_it::record_int_never_leaks_to_responses_audit_outbox_or_errors`）
  - [x] カーソルを改竄・別テーブル／別テナントへ流用すると復号または検証で拒否される
        （`cursor_tests::tampering_any_byte_is_rejected`・`reuse_on_other_table_or_tenant_is_rejected`）
  - [x] uuid キーと `record_int` キーで索引テーブルの大きさと、和集合・交差（Roaring）の速さを計測して記録する
        （[internal-keys-benchmark.md](../data/internal-keys-benchmark.md)）

### Task 15.2: pivot 索引テーブル導入＋二重書込＋オンラインバックフィル
- **area**: data / **path**: `crates/data/src/index/`, migrations, `crates/jobq`
- **依存**: 15.1
- **仕様**:
  - 索引テーブル群を追加（data-platform.md §3.3。すべて `PARTITION BY HASH (table_int)` 64 分割）:
    `data_index_text`（text/select/multi_select/date/datetime/*_ref/status＋システム擬似列）/
    `data_index_num`（number・`numeric`）/ `data_link`（record_ref）/
    `data_unique`（unique 制約を PK 1 本で賄う）/ `data_index_search`（部分一致・trigram GIN の隔離先）。
  - **索引の総本数はパーティションあたり 10 本の定数**とし、テナント数・テーブル数・列数に依存させない。
    （Phase 15 実装で変更: 数え方を改め **13 本**。`data_record` の `record_int` 一意索引と、15.3 の複合索引表
    `data_index_composite` の PK・値索引を含む。定数である点は同じ・data-platform §3.3）
  - **システム擬似列を `field_id` の予約枠に置く**（`0`=owner / `1`=created_at / `2`=updated_at、
    ユーザー列は `16..`）。全レコードに必ず 1 行あるため、**null 落ちしない全件駆動索引**として使える
    （任意列ソートで行が消える問題と、フィルタなし初期グリッドの両方がこれで解ける・§3.5）。
  - **リンクは 1 エッジにつき順方向行と逆方向行の 2 行**として持つ（同一 Tx で書く）。逆引きを
    `dst_table_int` 条件の専用索引で賄うと**逆引きだけ 64 パーティションを横断**し、N:N の主要経路が
    テーブル数に比例して劣化するため。両方向とも自分側の `table_int` に載るので枝刈りが効く。
  - **btree キー長の上限に対処する**。PostgreSQL はキーがページの 1/3（約 2,704B）を超えると
    `index row size exceeds maximum` で **INSERT 自体が落ちる**。現行の `MAX_TEXT_LEN = 10_000`
    （`crates/data/src/validate.rs`）はこれを超え得るので、索引テーブルには
    `MAX_INDEX_KEY_BYTES = 2_000` のプレフィクスのみ格納し `truncated` フラグを立て、
    等値・`StartsWith` は候補を本体で再確認する。`data_unique` は長値をハッシュ付き正規化キーにする。
  - **部分一致 GIN はスコープ列を索引自体に含める**（`btree_gin` で
    `(tenant_int, table_int, field_id, val gin_trgm_ops)`）。含めないと 1 テナントの検索でも
    プール全体の posting list を読み、ノイジーネイバーが p95 を壊す。
    **`pg_trgm`（`gin_trgm_ops` の提供元）と `btree_gin` の両方を先に `CREATE EXTENSION` する**
    （欠けると初回マイグレーションが失敗する）。マネージド PostgreSQL での拡張利用可否を
    プロビジョニングの前提条件に加える。
  - **`FieldId` の上限を `32_767` にする**（`MAX_FIELD_ID`）。`field_id` は索引エントリ幅のため `int2` で持つが、
    PostgreSQL に unsigned は無く `u16` の上位半分が保存できないため、採番時とスキーマ検証で強制する。
  - **リンクのカーディナリティを DB 制約で強制する**。`data_link` に `single_valued bool` を持たせ、
    部分一意インデックス `(tenant_int, table_int, field_id, src_record) WHERE single_valued` 1 本で
    両側を賄う（2 行方式なので「dst 側の一意」は逆方向行に対する同じ制約になる）。
    **さらにフラグ自体を書込側が指定できないようにする**——`data_link_field`（リンク列ごとの宣言
    メタデータ・スキーマ改訂時のみ書き換わる）への複合 FK で `single_valued` を宣言値に固定する。
    フラグを自由に渡せると `false` を指定して部分一意を迂回できてしまうため、ここまでやって初めて
    「DB で強制」になる。カーディナリティ変更はスキーマ改訂経路のみ（既存行の再検証つき）。
  - `data_record` も `PARTITION BY HASH (table_int)` へ移行し、PK を `(tenant_int, table_int, id)` にする。
  - **二重書込**: 書込トランザクションで旧 partial index と新索引テーブルの両方を更新する（切戻し可能に保つ）。
    索引更新は**変更のあったフィールドのみ**（v1 の `FieldPatch` を使う）。
  - 既存データのバックフィルを jobq のチャンクジョブで実装（冪等・中断再開可・進捗観測可）。
  - `COLLATE "C"` を採用（決定性・collation version 変更で索引が壊れるのを避ける）。
    **代償として漢字の並び順は符号位置順**になるため、読み仮名列でのソート指定を 15.5 の ViewSpec で受ける。
  - 多値列（`multi_select` / `link`）の 1 レコードあたり要素数上限を設ける（PIT-72）。
- **受け入れ条件**:
  - [x] 索引テーブルの本数がテーブル数・テナント数に依存しないことを、テーブルを大量生成する IT で確認する
        （`pivot_index_it::index_count_does_not_grow_with_tables_or_tenants`）
  - [x] バックフィルが中断・再開でき、完了後に旧索引と新索引の内容が一致する（parity 検証ジョブ）
        （`index_backfill_it::backfill_resumes_after_interruption_and_matches_old_index`・ジョブ経路は
        `index_backfill_job_it::sweep_and_jobs_initialize_backfill_and_verify_parity`。旧索引は 15.3 で撤去したので、
        比較の基準は本体に旧方式の式を当てた内容）
  - [x] 書込パスで変更のないフィールドの索引行が更新されない（差分更新の確認）（`pivot_index_it::write_path_updates_only_changed_fields`）
  - [x] 多値要素数の上限超過が 422 で拒否される（`pivot_index_it::multi_value_limit_is_enforced`。
        **Phase 15 実装で 400 に変更**: 入力値の検証は `DataError::Invalid` → 400・data-platform §5.6）
  - [x] `data_record` のパーティション枝刈りが効く（`EXPLAIN` で 1 パーティションのみ走査）（`pivot_index_it::partition_pruning_and_link_constraints`）
  - [x] **リンクの順引き・逆引きがいずれも 1 パーティションに枝刈りされる**（`EXPLAIN`）（同上）
  - [x] **`MAX_TEXT_LEN` 上限（10,000B）の値を持つレコードが `indexed` 列でも書き込める**
        （btree キー超過で落ちない）／切り詰め値の等値検索が本体再確認で正しく効く
        （`pivot_index_it::max_text_len_values_are_writable_in_indexed_columns`）
  - [x] 部分一致検索が他テナントのデータ量に影響されない（GIN スコープの確認）
        （`pivot_index_it::substring_search_is_scoped_inside_the_gin`・`query_plan_it::substring_search_is_driven_by_the_scoped_trigram_gin`）
  - [x] **拡張が無い環境で初回マイグレーションが明示的に失敗する**（`pg_trgm` / `btree_gin` の事前確認）
        （`pivot_index_it::missing_extensions_fail_the_migration_explicitly`）
  - [x] **`OneToOne` / `OneToMany` の一意性違反が DB 制約で弾かれる**（アプリ検証を迂回しても書けない）
        （`pivot_index_it::partition_pruning_and_link_constraints`）
  - [x] **`single_valued = false` を明示的に渡しても、宣言が `true` なら FK 違反で書けない**
        （フラグ経由の制約迂回ができないことの確認）（同上）
  - [x] `FieldId` が 32,767 を超える採番要求が 422 で拒否される（`pivot_index_it::field_id_above_int2_is_rejected`。
        **Phase 15 実装で変更**: 参照先側の一意の違反は unique と同じ 409 `unique_violation`（リンク列を名指し）、
        単一値の側の要素数の超過は 400・data-platform §5.6）
  - [x] **切り詰めが起きる列でのソートが既存契約（data-platform.md §3.3）どおり動く**
        （`(val_prefix, record_int)` のタプル比較で重複・欠落が無いこと、同一プレフィクス内の完全値順を保証しないこと、
        応答に `sort_precision: "prefix"` が含まれることを、同一プレフィクス多数のデータで確認）
        （`query_v2_it::truncated_sort_contract_and_precision_flag`）

### Task 15.3: クエリコンパイラ v2 ＋読取切替＋旧方式撤去
- **area**: data / **path**: `crates/data/src/query/`, `crates/data/src/policy/compile.rs`
- **依存**: 15.2
- **仕様**:
  - **クエリ IR を刷新**（data-platform.md §5）: `Condition` の AND/OR/NOT 条件木・型別演算子・
    複数ソートキー・**keyset カーソル（OFFSET 廃止）**・投影（`select`）・`group_by` ＋ 複数 `aggregate`・
    横断部分一致（`search`）。**演算子語彙は workflow IR の `Condition` に揃える**
    （`data.query` ノードで変換層が不要になる）。`matches`（正規表現）は索引が効かず DoS 面のため**採用しない**。
  - **行述語のコンパイル先を索引テーブル条件に変える**。`PolicyExpr` の AST・材料解決（`material.rs`）・
    fail-closed の閾値はそのまま。`IsOwner` は owner 擬似列（`field_id=0`）で評価する。
    **`row_policy` が参照するフィールドは `indexed` 必須**とし、スキーマ検証で強制する（data-platform.md §6.3）。
  - **戦略の選択をコンパイラが行う（自前オプティマイザの最小構成 O1・[query-optimizer.md](../data/query-optimizer.md) §10）**。
    プランナ任せにしない（PIT-69）。O1 の範囲:
    - **クエリ条件の意味論を 2 値論理で固定**（2026-10 決定。値なしの `Neq` は真・`Not` は補集合・多値列の `Eq` は「含む」・
      数値は数値として等値。§3.1。workflow の `eval.rs` 側の合わせ込みは 15.7）。
      **行述語は v1 の意味論のまま**（値なしの `Ne` は偽。§3.3）。これらに基づく正規化（否定の押し下げ・平坦化・定数畳み込み・同一フィールドの併合。§3.2）
    - 統計表は持たず、**計画時の上限付き件数探索**（上限 1,000・1 計画 3 本まで）と
      その結果のプロセス内短期キャッシュ（既定 60 秒。統計は認可材料ではない）で戦略を選ぶ。
      ただし**行述語の選言（`owner = me` 等）の探索はキーが材料から作られるのでリクエストを跨いでキャッシュしない**。
      値頻度表は outbox 消費側（15.6）を要するため 15.12 に回す
    - 戦略は**ソート駆動（任意列は 2 区間走査）・フィルタ駆動・認可駆動・OR 駆動**の 4 つ。認可駆動は
      見える行がごく少ない利用者向けで、Postgres には選べない。OR 駆動は別の列どうしの OR（各項が絞れているとき）を
      各項の索引範囲の和集合から入る
    - **プランナ耐性 SQL**（`OFFSET 0` で `EXISTS` の半結合化を止める・`CASE` で検査順を固定・
      `MATERIALIZED` で候補集合を囲う）と、**予算付きの切替**（次善の戦略の推定コストを予算とし、
      超えたら途中結果を捨てて選び直す・1 リクエスト 1 回まで）
    - **カーソルは戦略非依存**。ライブページングではソートキーの値・`record_int`・区間番号を含め、
      スナップショット読取ではこれらに `snapshot_seq` を加えて基準位置に束縛する（data-platform.md §3.3）
    - 切り詰めが起きるソート列は data-platform.md §3.3 の契約に従う（プレフィクス順で走査し同一プレフィクス内の完全値順は
      保証しない・keyset は `(val_prefix, record_int)`・応答に `sort_precision: "prefix"`）
    - **`Guard(P)` を型で強制**し、認可駆動でも行述語の検査を冗長に残す。**本体取得後に Rust で行述語を
      独立に再評価**し、不一致は落として警報（PIT-76）
    - 推定値・統計値・戦略名を利用者へ返さない。UI の値候補に値頻度表を使わない（PIT-75）
  - 索引到達可能性の強制（§5.2）: filter/sort/group_by/aggregate は `indexed || unique` 宣言済みのみ、
    マスク列は 403、`Contains`/`EndsWith` は `searchable` 宣言済みのみ、`statement_timeout`。
    条件は「**駆動索引が 1 つ以上決まること**」とし、「条件木に索引可能な述語を最低 1 つ」という
    強い形にはしない（`filter = None` の初期グリッド＝15.5 の要件が常に拒否されるため）。
    システム擬似列は常に駆動になれるので初期グリッドは成立する。
  - **複数ソートの最悪ケースを設計に含める**。pivot 索引はフィールドごとに別行なので複合順序の btree が
    存在せず、第 1 キー同値群が大きいと第 2 キー以降の再ソートが要る。既定は同値群をページサイズの
    K 倍（既定 8）まで先読みしてページ内ソート、超過時は複合索引をオンデマンド材料化する。
  - 件数は**上限付き正確カウント**（`Exact(n)` / `AtLeast(10_000)`）。上限はテナント設定で変更可能。
    **既定では数えない**（`with_count: false`）。グリッドは件数を別リクエストで非同期に取る（data-platform.md §5.4）。
  - **ページ取得中に行が変わったときの契約を分ける**。keyset が決定的なのは静的な集合に対してであり、
    ライブデータではソートキーの更新・削除で行が前後に移動する。
    ①**ライブページング（既定）は重複・欠落を保証しない**（SSE が同一セッションで補正する）
    ②**スナップショット読取**は `snapshot_seq` にカーソルを束縛し、その時点の集合に対して保証する
    （エクスポート・集計・ワークフロー一括処理用。保持期間外は 410）。
    各ページに `snapshot_seq` を添えて返し、クライアントが SSE の `delivery_seq` と突き合わせられるようにする。
  - **機能フラグで読取経路を切替**。切替前に ①同一クエリの新旧結果 parity 検証
    ②`policy_threat_it.rs`（781 行）が**両実装で通る**ことを必須にする。
  - 切替完了後、partial index を DROP し `crates/data/src/index.rs`（旧）と `data_index_registry` を撤去する。
    **これによりランタイム DDL が完全にゼロになる**（v1 が掲げた不変条件が初めて文字通りになる）。
- **受け入れ条件**:
  - [x] 条件木（AND/OR/NOT）・複数ソート・keyset ページングが動き、深いページでも応答時間が一定
        （`query_v2_it::random_queries_match_reference_under_every_strategy`・深いページ 11〜60 ページ目の p95 8.3ms は benchmarks.md §1）
  - [x] 100 万行のテーブルでグリッド 1 ページ（100 行）が p95 < 100ms（benchmarks.md §1: 初期グリッド p95 12.7ms）
  - [x] `policy_threat_it.rs` が新旧両実装で通る／parity 検証が全代表クエリで一致
        （CI ジョブ `data-strategies` が `SHIKI_DATA_READ_PATH=body`（本体経路＝旧方式）を含む全経路で回す・
        `query_v2_it::random_queries_match_reference_under_every_strategy` が本体経路とも突き合わせる）
  - [x] **EXPLAIN 回帰テストが CI にある**（選択的フィルタ×非選択的ソート／その逆／多条件 AND／OR 木／
        keyset 継続／**任意列ソート**／**第 1 キーが偏った複数ソート**の各パターンで駆動索引と実測行数を
        アサート・PIT-69）（`query_plan_it::plan_shapes_are_fixed_per_strategy`）
  - [x] **任意列（null あり）でソートしても可視行が欠落しない**（未設定レコードが結果から消えない）
        （`query_v2_it::nullable_sort_keeps_rows_without_value_at_the_end`）
  - [x] `filter = None` の初期グリッド（フィルタなし 100 行）が拒否されず p95 < 100ms で返る（benchmarks.md §1: 12.7ms）
  - [x] 別の列どうしの OR（各項が数百件）で OR 駆動が選ばれ、ソート駆動より速い（計測）
        （benchmarks.md §1: OR 駆動 p95 18.4ms／ソート駆動に決め打ち 362.4ms・`query_v2_it::budget_switch_and_union_drive_are_observable`）
  - [x] `with_count` 未指定のページ取得で件数クエリが発行されない（`query_v2_it::count_is_opt_in_and_scan_metrics_are_recorded`）
  - [x] リクエストごとに「走査した索引エントリ数 ÷ 返した行数」が記録され、テーブル単位で集計できる（同上・`TableScanMetrics`）
  - [x] **全戦略を強制実行して結果の集合と順序が一致する**（コストモデルは速さにだけ効く・PIT-76）。
        `policy_threat_it.rs` も全戦略で通る（`query_v2_it::random_queries_match_reference_under_every_strategy`・CI `data-strategies`）
  - [x] 正規化規則ごとの前後一致・TLP（`Q = (Q∧p) ∪ (Q∧¬p)`）・参照実装（擬似列全走査＋Rust 評価）との
        乱数比較（偏り・相関・値なし・多値・切り詰め値を含むデータ）が通る
        （`query/planner/normalize_tests.rs`・`query_v2_it::tlp_partitions_hold_through_sql`・`random_queries_match_reference_under_every_strategy`）
  - [x] 予算を極端に絞って切替を必ず起こしても結果が一致する（`query_v2_it::budget_switch_and_union_drive_are_observable`・CI の `SHIKI_DATA_SORT_BUDGET=1`）
  - [x] query-optimizer.md §1.2・§1.3 の試算・実測ケース（選択的フィルタ／非選択的フィルタ／見える行 0.01% の利用者／
        フィルタ列とソート列の相関）で、**Postgres 任せの単発 SQL と比べた p95・p99** を記録する（benchmarks.md §1）
  - [x] 戦略ごとの計画の形（駆動索引・SubPlan であること・ハッシュ結合が無いこと・計画時に 1 パーティションへ
        枝刈りされること）が固定されている。読取用の接続は `plan_cache_mode = force_custom_plan`。
        汎用プランの形も `EXPLAIN (GENERIC_PLAN)` と `force_generic_plan` 下の `EXPLAIN ANALYZE EXECUTE` で確かめる
        （`query_plan_it::plan_shapes_are_fixed_per_strategy`・`policy_drive_plan_for_users_who_see_few_rows`）
  - [x] 予算付きソート駆動が 1 往復で「L 件そろった／予算切れ／索引の終端」を判定でき、
        同じ戦略の継続（倍々）では途中結果を保持し、別戦略への切替では捨てる
        （`planner_budget_it::doubling_keeps_partial_results_and_never_rescans`・`switching_discards_partial_results`）
  - [x] **値の無い行で `Ne` を含む行述語が不成立になる**ことが、SQL 側（Guard）と事後検査の両方で v1 と同一
        （`query_v2_it::policy_ne_on_missing_value_is_false_on_every_path`・`planner/verify.rs` の `ne_on_missing_value_is_false_like_v1`）
  - [x] 事後検査で落ちた行が 0 件（落ちた場合は警報が出ることもテストで確かめる）
        （`query_v2_it` が `post_filter_drops_total` = 0 を確かめる・警報は `planner/verify.rs` の `mismatches_are_dropped_and_alerted`）
  - [x] 統計表への読取経路が `query/planner/` の外に無く、API スキーマに推定値・戦略名のフィールドが無い（PIT-75）
        （`shard_safety_lint::optimizer_statistics_are_only_touched_by_planner_and_stats_writers`・`QueryResult` に該当フィールドなし）
  - [x] `row_policy` が未索引フィールドを参照するスキーマが 422 で拒否される（`query_v2_it::row_policy_on_unindexed_field_is_422`。
        **Phase 15 実装で 400 に変更**・data-platform §5.6。v1 から移行したテーブルは参照列を自動で indexed に昇格・§6.3）
  - [x] マスク列を filter/sort/group_by/aggregate/search に指定すると 403（PIT-19 継承）
        （`query_view_it::field_mask_hides_and_blocks_search`・`policy_threat_it`）
  - [x] 集計のスモールセル抑制と集計クエリ監査が v1 と同じ挙動（PIT-17 継承）
        （`query_v2_it::aggregates_match_between_paths_with_small_cell_suppression`・`query_ops_it::aggregate_queries_are_audited`）
  - [x] `grep -r "CREATE INDEX" crates/data/src` が 0 件（ランタイム DDL ゼロ）（`no_runtime_ddl_lint::data_crate_issues_no_runtime_ddl`。
        起動時の較正も migration 0098 の固定表を読むだけ）

### Task 15.4: リンク／ルックアップ／ロールアップ／数式＋Materialization 規則
- **area**: data / **path**: `crates/data/src/derived/`, `crates/data/src/formula/`
- **依存**: 15.3
- **仕様**:
  - **双方向リンク**を `data_link` の順引き PK と逆引き索引で実装する（1:1 / 1:N / N:N を `ord` の多値で表現）。
    対称フィールド（参照先に現れる逆リンク列）は**スキーマ上のメタデータ**とし、
    実体は同じ行を逆から読む（**二重書込をしない＝不整合が構造的に発生しない**）。
    （**Phase 15 実装で変更**: 15.2 で決めた**2 行方式**（順方向行と逆方向行を同一 Tx で書く・data-platform §3.3・§7.1）のまま実装した。
    逆引き専用の索引で「同じ行を逆から読む」と逆引きだけ 64 パーティションを横断するため。対称列はスキーマ上のメタデータで
    本体に値を持たない点は同じで、逆方向行を閲覧者本人の権限で読取時に引く。同一 Tx で書くので不整合は構造的に起きない）
  - **ロールアップ**（count/sum/avg/min/max/concat）と**数式**を追加。数式は**閉じた AST ＋ Rust インタプリタ**
    （自由文字列を SQL にしない）。算術は `numeric`。**依存 DAG をスキーマ保存時に構築し循環を 422 で拒否**（実装は 400・data-platform §5.6）。
    数式の `Field` / `Rollup` は参照名（key）で書き、`Round` を足した。`date_diff` は UTC の日付での日数（data-platform §7.3）。
  - （実装: 材料化するのは `Invariant` かつ `indexed` の列だけで、値は索引行としてだけ持ち表示は常に読取時計算。多段参照は一律
    `PerViewer`。ReBAC の変化による降格は `after_table_rebac_changed`（参照元の列も検査し直す）と読取入口の再検査。既存テーブルは
    `has_record_shares = true` で初期化・data-platform §7.4）
  - **`Materialization { Invariant, PerViewer, Volatile }` を導入する**（data-platform.md §7.4）:
    `Invariant`（全閲覧者・全時刻で同値と証明できる）だけが書込時材料化・索引可。
    `PerViewer` / `Volatile` は**読取時計算のみ・キャッシュ禁止・索引/ソート/フィルタ不可**。
  - **`Invariant` の判定には 4 階層すべてを見る**（1 つでも閲覧者依存なら `PerViewer`）:
    ①参照先テーブルの **ReBAC viewer 集合が参照元を包含すると証明できるか**（できないのが既定）
    ②参照先の `row_policy` ③参照先の `field_policy` ④参照先の**レコード個別共有タプルの存在**。
    多段参照は推移的に辿る。`Now` を含む式は `Volatile`。
  - **降格トリガは 4 階層すべての変化**（policy 追加・ReBAC 変更・個別共有の発生）。降格時は
    材料化データと索引行を**同一トランザクションで破棄**する。これを忘れると漏洩が残留する（PIT-71）。
  - **参照先レコードの変更でも材料化値は陳腐化する**（金額更新・行の作成/削除・リンク付け替え）。
    依存 DAG を**実データの逆依存追跡**にも使い、参照先の変更 Tx から影響する参照元を特定して
    同一 Tx で再計算するか世代付きジョブへ enqueue し、**再計算完了まで当該列をクエリ対象外にする**。
  - **リンクのカーディナリティをスキーマで宣言する**（`LinkDef { ref_table, cardinality, symmetric_field }`）。
    値表現は常に配列。`OneToOne` / `OneToMany` の一意性は `data_unique` 相当で強制する
    （v1 の `record_ref` は単一 UUID 文字列しか受け付けず N:N を作れなかった）。
    （**Phase 15 実装で変更**: `LinkDef { cardinality, symmetric_field, symmetric_key, reverse }`・参照先は `FieldDef.ref_table`・
    v1 の向きの `ManyToOne` を追加。一意性は `data_link` の部分一意索引（`single_valued`）で強制し、dst 側の一意が要るリンクには
    対称列を必須にした（「制約専用の逆方向行」案は不採用）。v1 の単一 UUID 文字列は受け付けて配列で保存し、既存値はオンラインで
    配列へ移行する（`link_values` ジョブ・`link_value_migration_it`）。応答は常に配列。data-platform §7.1）
  - lookup / rollup / 数式のすべてで**参照先テーブルの行ポリシーを閲覧者本人の権限で透過適用**する
    （PIT-20 の全面解決）。ロールアップのスモールセル抑制は参照先の `aggregate_min_rows` を継承。
- **受け入れ条件**:
  - [x] N:N リンクの両方向が引け、**両方向とも 1 パーティションに枝刈りされる**（EXPLAIN で確認）
        （`derived_it::many_to_many_link_reads_both_directions_with_partition_pruning`）
  - [x] `OneToOne` / `OneToMany` の一意性違反が 422 で拒否される（`derived_it::link_cardinality_violations_are_422`。
        **Phase 15 実装で変更**: 参照先側の一意の違反は unique と同じ 409 `unique_violation`（リンク列を名指し）、
        単一値の側の要素数の超過は 400・data-platform §5.6）
  - [x] `PerViewer` / `Volatile` 列への `indexed`/`unique` 宣言が 422 で拒否される
        （`derived_it::viewer_dependent_or_volatile_columns_cannot_be_indexed_and_cycles_are_rejected`。**400 に変更**）
  - [x] **参照先テーブルの viewer 権限を持たないユーザーに、参照先の全件から計算したロールアップが
        見えない**（テーブル ReBAC 迂回の negative IT・PIT-71）（`policy_threat_it::rollup_does_not_bypass_reference_table_rebac`）
  - [x] **材料化済みテーブルの参照先に `row_policy` を後付け／個別共有を 1 件作成／ReBAC を変更**した
        いずれの場合も、材料化データが破棄され値が読めなくなる（`policy_threat_it.rs` に追加・PIT-71）
        （`policy_threat_it::materialized_rollup_is_discarded_when_reference_gets_row_policy`・`…_when_reference_record_is_shared`・
        `…_when_reference_table_rebac_changes`・`…_when_table_rebac_changes`・`…_on_query_and_on_field_policy`）
  - [x] 参照先レコードの金額を更新すると、参照元の材料化ロールアップが再計算される
        （再計算完了までは当該列がクエリ対象外になる）（`derived_it::materialized_rollup_is_recomputed_and_excluded_until_done`・409 `derived_recomputing`）
  - [x] 見えない参照先の lookup/rollup 値が null / 抑制される（PIT-20）
        （`policy_threat_it::rollup_and_lookup_apply_reference_row_policy_per_viewer`・`lookup_does_not_leak_invisible_reference`）
  - [x] 数式の循環参照が 422 で拒否される（`derived_it::viewer_dependent_or_volatile_columns_cannot_be_indexed_and_cycles_are_rejected`。**400 に変更**）
  - [x] 金額の四則演算・集計が `numeric` で正確（浮動小数点誤差が出ない）（`derived_it::money_arithmetic_and_rollups_are_exact`）

### Task 15.5: 型付き ViewSpec ＋ フロント
- **area**: frontend / **path**: `crates/data/src/view.rs`, `web/src/app/(auth)/data/`, `web/src/components/data/`
- **依存**: 15.3
- **仕様**:
  - `DataViewBody.display`（サーバ非解釈の JSON）を **型付き `ViewSpec`** へ置き換える。
    `#[serde(tag = "kind")]` のタグ付き enum: `Grid` / `Kanban` / `Calendar` / `Gallery`
    （`Form` は v1 スコープ外・data-platform.md §15）。**Rust 型を単一ソースとし ts-rs で TS 生成**。
    （Phase 15 実装で変更: **utoipa → OpenAPI → `pnpm gen:api`** で生成した。ViewSpec は REST にしか現れないため・data-platform §8.2）
  - `ColumnConfig`（表示順・幅・固定・非表示・**ソート代替列**＝日本語の読み仮名列）・行グループ化・
    条件付き書式（閉じた条件木）。**サーバが検証**する（存在しない `field_id`・マスク列の表示指定・
    `PerViewer` 列でのソート指定を拒否）。**1 ビューあたりのルックアップ・ロールアップ列は既定 12 本まで**
    （テナント設定で変更可・超過は 422。SharePoint のルックアップ 12 結合に倣う・data-platform.md §8.2）。
  - 保存は v1 どおり `artifact(kind=data_view)` の枠（ReBAC 共有・不変バージョン）。
    **実行は必ず `run_query` 経由**で閲覧者本人の権限で毎回再評価（作成者の権限を引き継がない）。
  - フロント: `/data` ルート（`web/src/lib/nav-config.ts` に追加）・テーブル一覧・**仮想化グリッド**
    （`csv-grid.tsx` の glide-data-grid＋ページキャッシュの前例を踏襲）・セル編集（`rev` 楽観ロック・409 は再読込）・
    スキーマエディタ（列の追加/並べ替え/表示名変更/索引トグル）・ビュー切替・フィルタ/ソート UI。
    API クライアントは `web/src/lib/data-api.ts`（型は `@/generated/api` から・手書き型を作らない）。
- **受け入れ条件**:
  - [x] グリッドで 100 万行のテーブルを無限スクロールでき、初回描画 p95 < 300ms（benchmarks.md §6: p95 133ms・手動 spec
        `web/e2e/data-first-render.manual.spec.ts`。スクロールバーで遠くの行へ直接は飛べない妥協は data-platform §5.3 に記録）
  - [x] 列の表示名変更・並べ替え・幅変更・非表示がビューに保存され、他ユーザーに影響しない
        （`view_spec_it::views_are_per_user_and_reevaluated_for_each_viewer`）
  - [x] マスク列や `PerViewer` 列を不正に指定した ViewSpec が保存時に拒否される（`view_spec_it::invalid_view_specs_are_rejected_on_save`）
  - [x] ルックアップ・ロールアップ列が 13 本以上のビューが保存時に 422 で拒否される
        （`view_spec_it::lookup_and_rollup_columns_are_capped_per_view`・`limit_exceeded`）
  - [x] 保存ビューを別ユーザーが開くと、そのユーザーの権限で行が絞られる
        （`view_spec_it::views_are_per_user_and_reevaluated_for_each_viewer`・`query_view_it::saved_view_reevaluates_per_viewer`）
  - [x] E2E（`web/e2e/data-grid.spec.ts`）でテーブル作成→列追加→レコード編集→ビュー保存が通る
        （同 spec の「テーブル作成 → 列追加 → レコード編集 → ビュー保存」）

### Task 15.6: レコード変更 outbox ＋ SSE 差分配信 ＋ 索引の自動昇格
- **area**: data / **path**: `crates/data/src/record.rs`, `crates/api/src/routes/data_stream.rs`
- **依存**: 15.3
- **仕様**:
  - `create_record` / `update_record` / `delete_record` に **outbox 書込**を追加する
    （現状 outbox を出すのは FSM 遷移のみ）。既存の per-consumer fan-out リレー
    （`crates/storage/src/event.rs`）を土台にする。
  - **配信シーケンスを新設する**。現行の `claim_undelivered` は `NOT EXISTS` の anti-join で、
    doc コメントどおり**意図的に単調な配信位置を持たない**（「id 順・コミット順に依存せず」）。
    `data_record_revision` の主キーもレコードごとの `rev` でストリーム位置ではない。このままでは
    `Last-Event-ID` と revision を対応付けられず、並行コミットや再接続で欠落・重複する。
    → **`outbox_delivery` に `delivery_seq` を追加**し `mark_delivered` 時に採番する。
    **グローバルな `bigserial` にはしない**——共有採番だと他購読者のイベントで番号に穴が空き、
    「連続確認済み」が定義できなくなる（穴を未確認とみなせば止まり、無視すれば取りこぼす）。
    `outbox_delivery_seq(consumer, next_seq)` から **consumer ごとの連番**を払い出し、
    **その行ロックが並行リレー間の排他も兼ねる**。
    outbox payload に `(table_int, record_id, rev)` を載せて配信位置と revision を永続的に対応付ける。
    保持期間外の `since` には `stream.reset` を返す（黙って欠落させない）。
    （**Phase 15 実装で変更**: 連番は consumer（購読者）ごとではなく**テーブルごとのストリーム**（consumer `data-stream:<table_id>`）。
    リレーは台帳 consumer `data-stream` として outbox を claim し、その採番行のロックでリレー間を排他する。購読者ごとの差は配信時の
    可視性判定で付けるので、見えない行の番号はクライアントに届かない。したがって low-water mark は**サーバ側**が持ち、
    クライアントの `Last-Event-ID` は最後に受けた `id` でよい。`table_int`・`record_int`・旧値は汎用の consumer が読まない
    非公開部 `private_payload` に置く。保持期間は `retain_until`（既定 600 秒）。data-platform §9.2）
  - **採番だけでは足りず、購読者ごとの送信順序も契約にする**。`claim_undelivered` は順序非依存なので
    リレーが `delivery_seq=10` を先に送り `9` を後に送り得る。10 で切断されると `since=10` の再開で
    **9 が永久に欠落する**。①リレーは 1 購読者への送信を `delivery_seq` 昇順に揃える
    ②再開カーソルは**連続確認済みの low-water mark** だけを進め、飛び番では進めない
    ③クライアントへ返す `Last-Event-ID` はこの low-water mark であり受信済み最大値ではない。
  - **配信の確定点と再送を契約にする**（data-platform.md §9.2）。`delivery_seq` の払い出し（`mark_delivered`）は
    **ストリームへの永続化**であってネットワーク送信の成功ではない。SSE の送信は永続化の後に行い、
    クライアントへの到達は **at-least-once**（再接続時に `Last-Event-ID` 以降を再送する）。重複は
    **クライアントが `(record_id, rev)` で冪等に捨てる**（既知の `rev` 以下の upsert／既に消した行の removed は無視）。
    クラッシュ窓ごとの結果: ①永続化前 → outbox に未配信で残り再 claim（欠落なし）②永続化後・送信前 → 再接続で再送
    ③送信後・クライアントの確認前 → 再接続で再送され冪等に捨てられる。
  - `GET /data/tables/{id}/stream?since=<delivery_seq>`（SSE）。
    **購読者ごとに行述語とフィールドマスクを再評価してから配信**する（見えない行・列は流れない）。
  - **可視性を失った行にも必ずイベントを送る**。更新後の値だけで判定すると、可視→不可視に変わった
    購読者には何も届かず**ブラウザが以前の機密値を表示し続ける**。before/after 双方で可視性を判定し、
    可視→不可視・削除では **`record.removed`（record_id のみ・値を含まない）** を送る。
    before 判定に要る旧値（旧 owner・旧述語参照列）は outbox payload に載せる。
  - ファンアウト最適化: **250ms 窓でバッチ**＋**述語をロール共通部とユーザー固有部に分解**してから
    グループ化する。素朴に SQL＋バインドのハッシュで束ねると、`IsOwner` は principal id が、
    個別共有は `shared_ids` がバインドに入るため**購読者数だけグループができて削減が効かない**。
    ロール共通部のみ SQL 評価をグループ共有し、ユーザー固有部はイベント側の `owner`／共有 id と
    購読者が持つ集合を突き合わせる O(1) 判定で済ませる。
  - **権限材料の TTL**: SSE 購読中のみ TTL（既定 5 秒）付き再解決を許し、
    **「権限剥奪の配信反映は最大 TTL 分遅れる」を製品の約束として文書化**する。
    REST 読取経路は従来どおり毎回解決（例外を SSE に閉じる）。ロール変更・共有解除・権限剥奪イベントを
    検知した購読は TTL を待たず即時無効化する（PIT-70）。
  - **索引の自動昇格**: 未索引列でのフィルタ/ソート要求を 403 で終わらせず、
    **2 万行未満のテーブルは即座にバックフィルして透過的に有効化**、それ以上は jobq で
    進捗表示つきバックフィル（完了まで「準備中」表示）。契機はビューの保存とグリッドでの並べ替え・絞り込み。
    （実装: 契機はビューの保存と /data グリッドのクエリの `promote_indexes: true` だけで、チャット・ワークフロー・ミニアプリの
    経路は昇格しない。閲覧者でも上限内なら昇格でき、承認は owner だけ。2 万行以上の降格はジョブ（`data_index_purge`）で片付け、
    後片付け中の再昇格と準備中の列を要するクエリは 409 `index_preparing`・data-platform §10）
    **テーブルあたりの索引列数は既定 20 列まで**（擬似列を除く・超過は明示承認・PIT-72）。数値は SharePoint Online を叩き台にした
    （data-platform.md §10）。
  - **索引状態を `absent` / `building` / `active` の 3 値にし、`building` 中は write-through する**。
    「進行中はクエリに使わない」だけでは競合を防げない——走査済み領域のレコードが有効化前に更新されると、
    その変更は**完成後の索引から永久に欠落する**。`building` に遷移した時点から新規書込を索引へ二重反映し、
    スナップショット走査と差分が収束してから原子的に `active` へ切り替える。
  - CRDT（Yjs）は**採らない**。行・列単位で可視性が異なり authz モデルと衝突するため
    （`crates/collab` はノート/スライド用途に留める）。編集はサーバ権威＋`rev` 楽観ロック。
    他ユーザーのカーソル/選択範囲は awareness チャネル（データを含まない）で配信。
- **受け入れ条件**:
  - [x] 他ユーザーの編集が 1 秒以内にグリッドへ反映される（`record_stream_it::other_users_edit_reaches_the_grid_within_one_second`・
        本番と同じ常駐タスク（250 ms 窓）で計測）
  - [x] **見えない行・マスク列が SSE に流れない**（negative IT）（`record_stream_it::invisible_rows_and_masked_columns_never_reach_the_stream`・
        `policy_threat_it::sse_never_streams_invisible_rows_or_masked_fields`）
  - [x] **可視→不可視に変わった購読者へ `record.removed` が届き、以後その行が表示されない**（negative IT）
        （`record_stream_it::visible_to_invisible_sends_removed_without_values`）
  - [x] 再接続時に `Last-Event-ID` から欠落・重複なく再開できる（並行コミットを含むシナリオ）／
        保持期間外は `stream.reset` が返る（`record_stream_it::resume_from_last_event_id_without_gaps_under_concurrent_commits`・
        `beyond_retention_gets_stream_reset`）
  - [x] **配信のクラッシュ窓（永続化前／永続化後・送信前／送信後）それぞれで、再接続後に欠落が無く、
        重複はクライアントの `(record_id, rev)` 判定で捨てられる**（故障注入つきの結合テスト）
        （`record_stream_it::crash_windows_lose_nothing_and_duplicates_are_idempotent`）
  - [x] **飛び番を受信した直後に切断しても、間の `delivery_seq` が再開後に配信される**
        （low-water mark 方式の確認・順序非依存リレーとの組み合わせで欠落しない）
        （`record_stream_it::low_water_mark_never_skips_an_uncommitted_gap`。**Phase 15 実装で変更**: low-water mark はサーバ側が持つ
        ので、確かめるのは「未コミットの穴を飛ばして位置を進めない」こと。クライアントには見えない行の番号が届かないため、
        クライアント側の `prev + 1` 判定は採らない）
  - [x] 権限剥奪後 TTL 以内に配信が止まる／TTL を超えて配信が続かない／REST 経路は TTL の影響を受けない（PIT-70）
        （`record_stream_it::revocation_stops_delivery_within_ttl_and_rest_is_immediate`）
  - [x] **`IsOwner` ＋ 個別共有を含む行ポリシーで、購読者 1,000 人・ロール 3 種のとき述語評価が 3 回**（計測）
        （`record_stream_fanout_it::thousand_subscribers_three_roles_evaluate_the_predicate_three_times`）
  - [x] バックフィル進行中の列がクエリ対象にならない／**`building` 中の書込が完成後の索引に反映される**
        （`query_ops_it::building_index_on_a_requested_field_is_not_ready`（409 `index_preparing`）・
        `index_promotion_it::large_table_promotes_in_background_with_write_through_and_demotes_by_job`・`index_backfill_it`）
  - [x] 2 万行未満のテーブルでは並べ替えから索引の有効化まで同期で完了し、21 列目の索引は明示承認を要求する
        （`index_promotion_it::small_table_sort_enables_the_index_synchronously`・`twenty_first_index_requires_explicit_owner_approval`）

### Task 15.7: 各面公開（workflow / shiki script / generative UI / chat）
- **area**: api / **path**: `crates/workflow-engine`, `crates/script-runtime`, `crates/gui`, `crates/chat`
- **依存**: 15.3, 15.4
- **仕様**:
  - **ワークフロー data ノードの解禁**: `NodeType::{DataQuery, DataRecordCreate, DataRecordUpdate,
    DataRecordDelete, DataTransition, DataBulkUpsert}` を `available_stage_a` へ入れ、
    `Scope::{DataRead, DataWrite}` を保存可能スコープに追加（現状 V3 が `ir.unknown_scope` で拒否）。
    `ir/params/data.rs` を新設し、`nodes/exec.rs` の dispatch・`NodePorts`・`ProdNodePorts` を配線。
    **IR `Condition` と `DataQuery.filter` が同語彙なので変換層を作らない**。
  - **workflow の評価器 `control/eval.rs` を data の意味論に合わせる**（2026-10 決定・query-optimizer.md §3.1）:
    多値の `Eq` / `Neq` / `In` は「含む／含まない」、`Contains` は要素のいずれかの部分文字列、空配列の `Exists` は偽、
    数値の等値は数値として比較（`1 = 1.0`）。既存の分岐の振る舞いが変わるので、**保存済み定義のうち配列値に
    `eq` / `neq` / `in` / `contains` を使う条件を切替前に検出して一覧化**し、ワークフローの検証器にも同じ検出を入れて
    保存時に警告する。
    書込は `effect_journal`（`IdempotencyClass::EngineDedup`）を通す。
    ノードカタログへ登録すれば `EmitWorkflowTool` の description に自動反映される。
  - **shiki script（ワークフロー/skill 経路）**: `HostBridge::dispatch`（`nodes/script.rs`）に
    `data.*` アームを追加する。api 名は既に `ALLOWED_APIS` にあるため追加不要。
    ゲスト側は `MINIAPP_PRELUDE` 方式（wasm 再ビルド不要）を踏襲する。`data.transition` も公開する。
    （実装: ノード・`Shiki.data.transition` とも遷移名ではなく **テーブル UUID ＋ レコード ＋ `to`（遷移先の status）**
    ＋任意の `expected_rev` で指定する。data の FSM が「現在の status → `to`」で定義されるため。前置きは
    `script_runtime::prelude` に共通化した）
  - **generative UI**: `ActionBinding` に第 4 種 `DataView { view_id, pinned_version }` を追加し、
    `data_grid` コンポーネントをデータ束縛で描画する。**クライアントが送れるのは `action_id + params` のみ**
    という `ActionDispatcher` の不変条件を維持し、束縛定義はサーバが照合する。
    `SpecValidator` が保存時に閲覧権限を解決してバージョンをピンする（`Workflow` 束縛と同じ流儀）。
    （**Phase 15 実装で変更**: #604 で tool 束縛を撤去したので `DataView` は束縛 3 種のうちの 1 つ。描画先は
    `TableProps`（静的データ）ではなく**ミニアプリの画面の `` ```data_grid ``**（中身は束ねた保存ビューの別名）で、
    ピンは `MiniAppBody.data_views` に持ち、**`MiniAppStore` の保存時**に保存者の権限で解決する。描画は
    `POST /mini-apps/{id}/ui-actions`（`action_id = data_grid:{別名}`・params は `cursor` / `limit` だけ）。会話の回答内の表の
    データ束縛は未実装・data-platform §11）
  - **chat / agent-core**: `data_query`（読取）・`data_record_write`（`requires_confirmation = true`）・
    `data_schema`（グラウンディング用）を追加する。`csv_tool.rs` の 3 ツールが最も近い先例。
    description には**発話ユーザーが viewer のテーブル一覧を動的に載せる**（`SkillTool` と同じ流儀・
    テーブル数は有界）。`worker/toolset.rs` の「未配線なら提示しない」ポリシに従う。
- **受け入れ条件**:
  - [x] スケジュールトリガのワークフローが `data.query` → `data.record.update` を実行でき、
        **委譲元ユーザーの権限**で行が絞られる（`crates/api/tests/workflow_data_it.rs::scheduled_workflow_queries_and_updates_with_the_delegators_permissions`）
  - [x] shiki script から `Shiki.data.query` が呼べ、スコープ ceiling 外の呼び出しが `out_of_scope` で監査される
        （`crates/workflow-engine/tests/data_nodes_it.rs::script_shiki_data_query_and_out_of_scope_audit`）
  - [x] チャットで「経費テーブルの承認待ちを金額順に見せて」が動き、見えない行が返らない
        （`crates/chat/tests/data_tools_it.rs::chat_run_answers_pending_expenses_by_amount_within_speaker_permissions`）
  - [x] generative UI のテーブルがデータ束縛で描画され、**閲覧者本人の権限**で毎回再評価される
        （`crates/gui/tests/miniapp_data_grid_it.rs::data_grid_is_evaluated_with_each_viewers_own_permissions`。対象はミニアプリの画面の
        `` ```data_grid ``。会話の回答内の表は対象外＝未実装）
  - [x] 全面で同一の `DataQuery` 型が使われている（面ごとの独自フィルタ表現が存在しない）（`crates/api/tests/data_query_surfaces.rs`）
  - [x] **`eval.rs` と data の評価規則が同じ結果を返す**ことを、共通のテスト表（演算子 × 型 × 値なし／空／多値／数値）で固定
        （`semantics_table.json` を `control/eval_semantics_tests.rs`・`query/semantics_tests.rs`・`semantics_table_it.rs`（実 SQL）が読む）
  - [x] 配列値に `eq` / `neq` / `in` / `contains` を使う保存済みワークフローが切替前に一覧化され、保存時に警告が出る
        （`workflow_data_it::saved_workflows_with_array_conditions_are_listed`・`shiki-admin workflow-conditions`・
        保存／検証 API の `ir.condition_semantics_changed` とエディタの表示）

### Task 15.8: RAG 統合スパイク（構造化データの permission-aware 検索）
- **area**: rag / **path**: `crates/rag`（設計スパイク）
- **依存**: 15.3
- **仕様**:
  - テーブル単位で「検索対象にする」をオプトイン。レコード 1 件 = 1 チャンク。
    投影テンプレート（タイトル列・本文列）を指定する。
  - **pre-filter** は `data_table` を authz タグに使う（既存の folder/file タグと同型）。
  - **post-filter** は「候補 `record_id` を可視集合に絞るクエリ 1 本」で済む。
    **OpenFGA のタプルを 1 本も増やさずに行レベルの permission-aware 検索が成立する**
    （`crates/rag/src/authz_filter.rs` の file 粒度 post-filter を多型化する）。
  - **行の post-filter だけでは足りない**。チャンクは書込時に静的生成されるため、投影テンプレートに
    フィールドマスク対象列を含めると、行が見えるユーザーには**マスク列の内容まで回答・引用に流れる**
    （全社員に見える人事レコードの給与列を隠しても RAG 経由で出る）。v1 は fail-closed に倒し、
    **投影テンプレートにマスク対象列を含められない**ようスキーマ検証で拒否する。
    対象テーブルに `field_policy` が後から付いたら索引を破棄して再構築する。
  - 増分索引は 15.6 の outbox に相乗り。`SearchResult` の多型化（file 前提を崩す）が必要。
  - **本タスクは設計スパイクまで**。実装はポストアルファ可。
- **受け入れ条件**:
  - [x] `SearchResult` 多型化と post-filter 一般化の設計が docs に落ちている（[data/rag-integration.md](../data/rag-integration.md)）
  - [x] 行レベル権限が検索結果に効くことを検証する PoC がある（`crates/rag/tests/record_search_poc_it.rs`）
- **成果物**: 設計 [data/rag-integration.md](../data/rag-integration.md)。PoC は `crates/data/src/rag/`・
  `crates/rag/src/record_source.rs`・`crates/rag/tests/record_search_poc_it.rs`（実 PG ＋実 OpenFGA・
  取得段は in-memory の偽物）。本番の配線（sink 実装・`SearchService` の切替・API 公開）は同文書 §8 の残作業

---

## 参照

- 設計正本: [data-platform.md](../data-platform.md)／クエリオプティマイザ: [data/query-optimizer.md](../data/query-optimizer.md)
- 落とし穴: [design-caveats.md](../design-caveats.md) PIT-17〜21（v1 から継承）・**PIT-69〜76**（v2 が持ち込む）
- v1 の実装: [phase-9.md](./phase-9.md) Task 9.2〜9.5・9.10
- ワークフロー連携: [phase-10.md](./phase-10.md)（Stage B の data 系ノード）
- テナンシー前提: [design.md](../design.md) §4.1（フルプール）

### Task 15.9: 認可材料のリクエストスコープ memo ＋ BatchCheck ＋ スキーマキャッシュ
- **area**: data / **path**: `crates/data/src/policy/material.rs`, `crates/authz/src/client.rs`
- **依存**: 15.3
- **仕様**（data-platform.md §16.4・§16.6）:
  - グリッド 1 ページの読取で OpenFGA へ 2 往復（`list_objects(Role)` ＋ `list_objects(DataRecord)`）。
    ページ＋件数＋lookup 解決を合わせると **1 画面で 6 往復以上**になり、p95 < 100ms を食い潰す。
  - **PIT-18 の「キャッシュ禁止」はリクエストを跨いだキャッシュの話**であり、
    同一リクエスト内のメモ化は安全（処理中に権限が変わる前提を置く必要がない）。
    リクエストスコープの memo を入れる。**プロセス内・リクエスト跨ぎのキャッシュは引き続き禁止**。
  - `AuthzClient` に **`batch_check`** を追加する（現状 post-filter は `try_join_all` で個別 check）。
  - `data_table.schema` は全クエリで引かれる。**`schema_version` をキーにしたプロセス内キャッシュ**を置く
    （スキーマは権限ではないのでキャッシュ禁止の対象外）。
- **memo とキャッシュのキー契約**（緩めると PIT-18 を破る）:
  - `material::resolve` の memo キーには **`AuthContext` の関連部分を漏れなく**含める
    （`PrincipalKind` / principal ID / `org` / `tenant_id`）＋**解決対象の式集合**
    （`&[&PolicyExpr]` を取るため、式が違えば材料も違う）。
  - **生存範囲はリクエストスコープに厳密に閉じる**（型でリクエスト外へ持ち出せない形にする）。
  - スキーマキャッシュのキーは **`(tenant_id, table_id, schema_version)`**。
    `schema_version` は**テーブルごとに 1 から始まる**（`migrations/0033_data_service.sql` の
    `default 1`）ため単独では一意でない。改訂・削除・**テナント purge** で世代を原子的に切り替える。
  - **スキーマキャッシュに認可材料を混ぜない／プランキャッシュにも混ぜない**（§16.3・§16.4）。
- **受け入れ条件**:
  - [x] グリッド 1 画面の読取で OpenFGA 往復が 2 回以下（計測）（`authz_memo_it::grid_screen_reads_with_at_most_two_fga_round_trips`:
        viewer・参照先 viewer・名指しロールの BatchCheck 1 回＋個別共有の list_objects 1 回）
  - [x] **リクエストを跨いだ材料の再利用が無いことをテストで固定**（権限剥奪が次リクエストで即反映・PIT-18）
        （`authz_memo_it::revocation_is_visible_on_the_very_next_request`・`policy/memo_tests.rs` の `next_request_sees_revocation`）
  - [x] **別テナント・別テーブル・別 principal・別式集合で材料が混ざらない**（memo キーの網羅テスト）
        （`memo_tests.rs` の `every_context_part_is_in_the_key`・`expression_set_is_in_the_key`・
        `authz_memo_it::materials_of_different_tables_do_not_mix_within_a_request`）
  - [x] スキーマ改訂後の最初のクエリが新スキーマで動く／**テーブル削除・テナント purge 後に
        古い世代が読まれない**（世代切替の原子性）（`authz_memo_it::schema_cache_switches_generation_on_revision_delete_and_purge`）

### Task 15.10: SQL 互換インターフェース ＋ 最適化層 ＋ プランキャッシュ
- **area**: data / **path**: `crates/data/src/sql/`, `crates/data/src/query/`
- **依存**: 15.3, **15.4**（リンク JOIN は `LinkDef` を要するため。15.4 未完なら JOIN 抜きで先行可）
- **仕様**（data-platform.md §16.1〜§16.3）:
  - **SQL を前段言語として受ける**。`sqlparser`（既にワークスペース依存）でパースし、
    閉じた部分集合に照合してから **`DataQuery` IR へ変換**する。
    **不変条件: 呼び出し側の SQL テキストを 1 文字もデータベースへ届けない**（IR が全経路の合流点）。
    前例は Salesforce SOQL（pivot ストア上の SQL 面）と `crates/tabular/src/sql_guard.rs`。
  - 受け付ける部分集合: `SELECT`（`*` 禁止）/ `FROM` / **宣言済みリンク列経由の `JOIN` のみ** /
    `WHERE`（IR の閉集合）/ `GROUP BY` / `HAVING` / `ORDER BY` / `LIMIT`（`OFFSET` 禁止）。
    拒否: DML・DDL・CTE・ウィンドウ関数・相関サブクエリ・`UNION`・任意関数・`pg_*`。
    **JOIN を制限する理由は authz**——任意結合だと参照先の行ポリシー伝播が破綻する。
  - **JOIN は文法上そう書けるだけでは不十分**。`ON` 式は「宣言済みリンク列の等値 1 個」のみ許し、
    任意式・追加条件・別名経由の未宣言結合を拒否する。束縛先は `(src_table_int, field_id, dst_table_int)`
    が `LinkDef` と一致するもののみで、カーディナリティも `LinkDef` から取る（SQL 側で上書きさせない）。
    参照先の行述語を必ず適用する。多段結合には `MAX_JOIN_DEPTH` を置く。
  - **入口の資源上限を置く**（`statement_timeout` は DB 実行時間の上限でしかない）:
    `MAX_SQL_BYTES` 64KB / `MAX_AST_DEPTH` 32 / `MAX_AST_NODES` 4096 / `MAX_LITERALS` 1024 /
    `MAX_COMPILE_WORK` 200 万単位（コンパイルの仕事量・決定的）/ `MAX_COMPILE_MS` 500ms（壁時計の外側の安全網）。
    **IR 直投入の経路にも同じ上限を課す**。
  - **検証は allowlist（閉じた部分集合）で行う。** `crates/tabular/src/sql_guard.rs` は
    「非 Query 文の拒否＋危険関数の denylist」であり **方針が逆**なので踏襲しない
    （あちらはランナーの `enable_external_access=false` という第 2 の壁が前提）。
  - 拒否理由を構造化して返し、LLM が自己修正できるようにする（`EmitUiTool` と同じ流儀）。
    ただし**存在しない列・見えない列・存在しないテーブル・見えないテーブルは同一メッセージ**にする
    （区別すると列の存在オラクルになる）。
  - **最適化層**: 述語の並べ替え・射影刈り込み・**JOIN の半結合化**・恒真恒偽の畳み込み。
    **「JOIN 除去」と書かない**——`INNER JOIN` は参照先が無い行を除外し多対一以外では行を重複させるため、
    単に消すと件数と集計値が変わる。正しい変換は**認可述語つき `EXISTS` への半結合化**で、
    安全と証明できない場合は結合を残す。
  - **プランキャッシュ**: クエリ**形状**（条件木構造・ソートキー・射影集合・`HasRole` の畳み込み結果・
    スキーマ由来の型/フィールド構造・コンパイラのバージョン）をキーにし、
    **同一形状は必ずバイト同一の SQL テキストを生成**する。現在の `format!` 15 箇所は
    テキストが変動し、Postgres の parse/plan も sqlx の prepared statement キャッシュも効かない。
    **認可材料をプランキャッシュへ混ぜてはならない**（他人の材料が載ったプランを引く事故になる）。
    `tenant_id` / `table_int` はバインド値なのでキーに不要。
- **受け入れ条件**:
  - [x] **SQL テキストがデータベースへ渡らないことをテストで固定**（IR 経由が必須・negative test）
        （`sql_plan_it::caller_sql_text_never_reaches_the_database`・DB との間に TCP の中継を挟んで送信文を全て記録）
  - [x] 部分集合外の SQL（DML・CTE・ウィンドウ関数・相関サブクエリ・`UNION`・未 allowlist 関数・`pg_*`）が
        422 で拒否され、理由が構造化されて返る（`sql_it::subset_violations_are_422_with_structured_reasons`）
  - [x] 行述語・フィールドマスク・集計抑制が SQL 経路でも同一に効く
        （`policy_threat_it.rs` を SQL 面でも実行し、IR 直投入と結果が一致すること）（`policy_threat_it` の各シナリオが
        `sql_mirror` で SQL 面の結果を IR 直投入と突き合わせる）
  - [x] **各資源上限を超える入力が上限ごとに 422 で弾かれる**
        （`MAX_SQL_BYTES` / `MAX_AST_DEPTH` / `MAX_AST_NODES` / `MAX_LITERALS` / `MAX_COMPILE_WORK` / `MAX_COMPILE_MS`）
        （`sql_it::each_resource_limit_is_422_on_sql_and_ir_paths`・`compile_work_budget_passes_the_static_worst_case_and_cuts_superlinear_cost`）
  - [x] **存在しない列・見えない列・存在しないテーブル・見えないテーブルの応答がバイト単位で同一**
        （列/テーブルの存在オラクルを作らない・negative test）（`sql_it::unknown_and_invisible_names_get_byte_identical_responses`）
  - [x] **任意 `ON` 式・追加条件・別名経由の未宣言結合が拒否される**／JOIN に参照先の行述語が必ず掛かる
        （`sql_it::joins_bind_only_declared_link_columns_and_apply_reference_row_policy`）
  - [x] **半結合化の前後で件数と集計値が変わらない**（1:N リンクを含むデータで検証）
        （`sql_it::semijoin_rewrite_keeps_counts_and_aggregates_on_one_to_many_links`）
  - [x] 同一形状のクエリが同一の SQL テキストを生成する（prepared statement が再利用される計測）
        （`sql_plan_it::same_shape_is_byte_identical_across_principals_and_prepared_statements_are_reused`・benchmarks.md §4）
  - [x] **プランキャッシュに認可材料が混入していない**（別 principal で同一形状のプランを引いても
        材料はバインド経由でのみ渡ることをテストで固定）（同上。`HasRole` の成否も真偽のバインドで渡し、キーに入れない・data-platform §16.3）
  - [x] **SQL 入力の生成成功率**: 評価用クエリ集合（業務的に代表的な 50 問。単純フィルタ／複合条件／
        集約／ソート＋ページング／リンク経由の 5 カテゴリ × 10 問）で、**IR 直書きの基準値を先に測定**し、
        SQL 入力が ①初回正解率で IR を下回らない ②リンク経由カテゴリの失敗が IR より少ない
        ③1 問あたりの出力長が IR の 70% 以下 ④構文エラーが IR 以下、をすべて満たすこと。
        失敗は「構文エラー／部分集合外／列・テーブル名誤り／意味的に不正（意図と異なる結果）」に分類して記録する。
        **2026-10-23 改訂**: 当初の「+15 ポイント以上」は、IR の基準値がすでに 98% 前後で天井に近く
        達成不能だったため、human の判断で上記に置き換えた。実測は [sql-eval.md](../data/sql-eval.md)
        （IR 98.7% → SQL 99.7%・リンク経由の初回失敗 IR 8 件 → SQL 0 件・出力長 約 50%・構文エラー IR 3 件 → SQL 0 件）

### Task 15.11: 一括書込パス
- **area**: data / **path**: `crates/data/src/bulk/`, `crates/api/src/routes/data_imports.rs`, `crates/jobq`
- **依存**: 15.2
- **仕様**（data-platform.md §16.7）:
  - 10 万行インポートは本体 10 万行 ＋ 索引 80 万行。1 行ずつ INSERT では成立しない。
  - **COPY を authoritative table へ直接流さない。** 速いが、通常書込が担保する不変条件が
    まとめて抜ける（認可・unique・リンクのカーディナリティ・`rev`・outbox/SSE・材料化・索引 parity）。
    **①一時ステージング表へ COPY → ②通常書込と同じ検証器で正規検証 → ③チャンク Tx で適用**
    の 3 段にする。③では本体・索引行・revision・outbox を通常経路と同じ規則で生成し、
    `Invariant` 計算列の材料化も走らせる。
  - **チャンク境界で冪等**（取込 ID ＋ 行キーで二重適用を防ぐ）。
  - **索引の遅延構築**は §10 の `building` 状態を流用し、**`building` 中も write-through** する
    （一括取込中の並行書込を取りこぼさない）。
  - **Phase 15.2 のバックフィル自体がこの経路を要求する**ため、15.2 と同時か先行して着手する。
  - （実装: 適用は 1 件の作成と同じ入口 `record_effects` を通るので outbox（SSE）・材料化・監査も同じ規則で出る。取込は新規作成で、
    既存レコードの更新を混ぜるのはワークフローの `data.bulk_upsert`（15.7）。HTTP API は `POST /data/tables/{id}/imports`（JSON）・
    `…/imports/csv`（CSV）・進捗と行エラーの `GET`。本文の上限 32 MiB・行数の上限 100 万行・data-platform §16.7）
- **受け入れ条件**:
  - [x] 10 万行のインポートが所定時間内に完了し、途中で他テナントの p95 を悪化させない
        （`bulk_import_it::bench_100k_import`・benchmarks.md §3: **所定時間は 10 万行で 120 秒**と定め（根拠は同節）、実測 64〜77 秒。
        「悪化させない」は取込中の p95 ≤ 平常の p95 × 1.25 ＋ 2ms と定めて満たす）
  - [x] 中断・再開でき、再実行が冪等（チャンク境界で二重適用が起きない）（`bulk_import_it::bulk_import_survives_interruptions_and_duplicate_workers`）
  - [x] **一括経路が通常書込と同一の不変条件を満たす**——認可・型・required・unique・
        リンクのカーディナリティ・多値要素数上限・`rev`・outbox/SSE 発火・`Invariant` 材料化
        （`bulk_import_it::bulk_import_matches_single_creates_and_rejects_invalid_rows`・`bulk_import_enforces_authorization_per_chunk`・
        `bulk_import_materializes_invariant_rollups_and_enforces_link_cardinality`・SSE は
        `record_stream_it::bulk_import_transition_derived_schema_and_awareness_reach_subscribers`）
  - [x] **取込後に索引 parity 検証が通る**（本体と索引の突合で差分ゼロ）（`bulk_import_it` の各テスト・ベンチの parity 不一致 0）
  - [x] `building` 中の並行書込が完成後の索引に反映される（`bulk_import_it::deferred_index_build_keeps_concurrent_writes`）

### Task 15.12: 自前クエリオプティマイザ O2/O3（統計収集・コスト較正・フィードバック・拡張戦略）
- **area**: data / **path**: `crates/data/src/query/planner/`, `crates/data/src/stats/`
- **依存**: 15.3（O1）, 15.6（outbox 消費側）, **15.4**（O3 のリンク多段・ルックアップ）
- **仕様**（[query-optimizer.md](../data/query-optimizer.md) §4〜§8・§10）:
  - **O2: 統計とコスト**
    - **行数カウンタと値頻度表**（種類数が上限以下の列は値ごとの正確な件数）を outbox 消費側でバッチ更新する
      （書込 Tx では更新しない。人気の値の 1 行に書込が集中するため）。冪等性は消費位置で担保する
      （実装: 台帳 consumer `data-stats` の配送記録（同じ Tx の `mark_delivered`）で担保・query-optimizer.md §4.1）
    - フィールド統計（値を持つ行数・異なり数・最頻値 100 件・等深ヒストグラム 100 区間・切り詰め率・多値の平均要素数）と
      リンク統計（方向ごとのファンアウト）・行幅を、jobq の**統計収集ジョブ**で作る。起動は
      「前回からの変更行数 > 50 + 0.1 × 行数」・索引昇格の完了時・推定外れの継続時。テナントの公平性の枠で消費する
    - 統計 4 表（`data_table_stats` / `data_field_stats` / `data_value_count` / `data_link_stats`）を
      **tenant スコープと purge 経路の CI ゲート（Phase 12.9）の対象に入れる**
    - 推定は区間と信頼度で持つ。`All` は**指数バックオフ**（積にしない）
    - **コストを予測レイテンシ（µs）で表し、起動時の較正ベンチで単価を環境ごとに置き換える**
    - **頑健な選択**: 推定値で最小の戦略の最悪コストが、全戦略の最悪コストの最小値の 4 倍を超えるなら後者を選ぶ
    - **フィードバック**: `(形状, 戦略, 推定, 実測, 所要時間)` を記録し、`(テーブル, フィルタ列, ソート列)` ごとの
      補正係数（指数移動平均）で相関を吸収する。複合索引の材料化・索引昇格の候補を管理画面に出す
    - **予測コストによる受付制御と公平性**: 読取予算（既定 2 秒）を超える見込みのクエリは実行せず、
      絞り込みの追加か索引の昇格を提案する。§16.8 の同時実行制限を予測コストのトークンで消費する
    - 機微なテーブル向けに、**テーブル設定でコストベース選択を止めて固定戦略にできる**（PIT-75 の残余リスク）
    - **ベンチのデータは統計から合成する**: 実テナントのテーブルの形（行数・型・種類数・頻度の形・値なし率・列間の相関）を
      値を含まない数値として取り出し、それに合う疑似データを生成する。顧客データはベンチに持ち込まない
      （Garoon の先例・query-optimizer.md §9.3）
  - **O3: 拡張戦略**
    - **交差駆動**（等値範囲 2 つを `record_int` 順で突き合わせる）
    - **バッチ駆動**（`= ANY($ids)` で段ごとに一括検査）を、リンク多段の条件とグリッドのルックアップ解決
      （data-platform §16.9 の N+1）に使う。集合の和・積・差は `record_int` の Roaring ビットマップで持つ
      （実装: バッチ駆動は**自動選択では選ばず**、リンクを辿る解決と強制戦略だけで使う。交差駆動は自動選択の候補・query-optimizer.md §6.4）
    - （実装で追加: 較正は固定表 `data_planner_calib`（migration 0098・テナントに属さないシステム表）を読むだけでランタイム DDL を
      打たない。管理 API `/org-admin/data/tables/{id}/optimizer`・`/org-admin/data/optimizer` と管理画面。形の取り出しは
      `shiki-admin data-table-shape`・ベンチは `DATA_BENCH_SHAPE`）
- **受け入れ条件**:
  - [x] ベンチのデータが顧客データを含まず、実テナントの形（行数・型・種類数・頻度の形・値なし率・相関）だけから合成される
        （`optimizer_bench_it::bench_regret_calibration_and_fairness`・`optimizer_it::table_shape_is_extracted_without_values_through_the_cli_entry`・benchmarks.md §2）
  - [x] 偏り（Zipf）・相関・値なし・多テナントの雑音を含むベンチで、**後悔比**
        （実所要時間 ÷ 全戦略の最短）が代表クエリの p95 で 2 倍以内（benchmarks.md §2: p95 1.59 / 1.47）
  - [x] 較正ベンチの前後で単価が置き換わり、予測コストと実測の相関が記録される
        （`optimizer_it::calibration_replaces_unit_costs`・benchmarks.md §2.1）
  - [x] 相関のあるデータで、2 回目以降の実行で補正係数により正しい戦略が初手で選ばれる
        （`optimizer_it::correction_factor_picks_the_right_strategy_on_the_second_run`）
  - [x] 予算超過の見込みのクエリが実行されずに構造化エラーで返る（推定値はエラーに含めない・PIT-75）
        （`optimizer_it::over_budget_queries_are_rejected_without_estimates`・400 `query_too_expensive`）
  - [x] 統計収集ジョブが他テナントの読取 p95 を悪化させない（benchmarks.md §2.2・`optimizer_it::analyze_jobs_are_claimed_fairly_across_tenants`）
  - [x] O3 の各戦略も 15.3 の強制戦略テスト・TLP・`policy_threat_it.rs` に加わって通る
        （`query_v2_it::o3_strategies_run_and_match_reference`・`policy_threat_it::intersect_and_batch_strategies_apply_and_respect_row_policy`・
        CI `data-strategies` の `intersect` / `batch`）
