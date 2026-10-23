# 構造化データの RAG 統合（permission-aware 検索）

> **状態**: 設計スパイク＋ PoC（Phase 15 Task 15.8）。本番の配線（API・チャットの `doc_search`・
> ベクタストア/全文索引への書込）はポストアルファ。上位の方針は [data-platform.md §11.1](../data-platform.md#111-rag-統合方向性)。
>
> PoC のコード: `crates/data/src/rag/`（投影・世代・post-filter の入口・増分索引）／
> `crates/rag/src/record_source.rs`（候補の多型化・post-filter の一般化）／
> 結合テスト `crates/rag/tests/record_search_poc_it.rs`（実 Postgres ＋実 OpenFGA）。

## 0. 結論

- **テーブル単位でオプトイン**し、**レコード 1 件 = 1 チャンク**を投影テンプレート（タイトル列＋本文列）から作る。
- **pre-filter** はチャンクの構造タグ `data_table:<tenant>|<table_id>` ∩ `ListObjects(viewer, data_table)`。
  folder/file タグと同じ形で、テーブル共有の変更で索引を書き換えない。
- **post-filter** はテーブルごとに「候補 `record_id` を可視集合に絞るクエリ 1 本」。テーブル viewer・
  行述語（v1 の意味論）・個別共有を REST の読取と同じ合成で評価する。**OpenFGA のタプルは 1 本も増やさない**。
- **フィールドマスクは投影に入れられない**（スキーマ検証で 422）。`field_policy` や投影の定義が変わると
  索引の**世代**が変わり、索引を破棄して作り直す。作り直しが終わるまでの古いチャンクは post-filter が
  世代不一致で落とす（fail-closed）。
- 候補の型を `SourceRef { File | Record }` に多型化し、post-filter を file とレコードの混在に一般化した。
  ファイル検索の既存経路と判定規則は変えていない。

## 1. オプトインと投影テンプレート

`TableSchema.rag`（`data_table.schema` JSONB の一部・serde default で後方互換）で宣言する。

```json
{
  "fields": [ ... ],
  "row_policy": { ... },
  "rag": { "title_field": "title", "body_fields": ["memo", "dept", "amount"] }
}
```

| 規則 | 理由 |
|---|---|
| `title_field` 1 列＋ `body_fields` 0〜8 列。重複・存在しない列は 422 | 投影の形を閉じる |
| 投影できる型: text / number / date / datetime / select / multi_select / user_ref / role_ref | 保存値を持ち、閲覧者に依らず同じ値が見える |
| **`field_policy` の対象列は 422** | §4 のフィールドマスクの穴 |
| 派生列（lookup / computed / rollup / formula）は 422 | 参照先の 4 階層 authz を閲覧者本人の権限で読取時に計算する列で、全閲覧者共通のチャンクへ焼き込めない（PIT-20・PIT-71） |
| file_ref / record_ref / リンクは 422 | 値が参照先の id で本文として意味を持たない。参照先の可視性を焼き込む経路も作らない |

チャンク本文は「タイトル」と、本文列を宣言順に `表示名: 値` で改行連結したもの。null・欠落は行ごと省き、
多値は `, ` で連結する。タイトル＋本文で **4,000 文字**（`MAX_RAG_CHUNK_CHARS`）で切り詰める
（巨大チャンクは意味検索で希釈される・PIT-67）。索引側と post-filter 側は**同じ関数**で本文を作る
（`crates/data/src/rag/projection.rs` の `render`）。

検証は `validate_table_schema` の中で走るので、作成・改訂・ミニアプリのマニフェスト検証のすべてに効く。
エラーは `DataError::Invalid`（data-platform の他のスキーマ検証と同じ「422」扱い。なお現状の API 層は
`DataError::Invalid` を 400 に写している — 既存の挙動で本タスクでは変えていない）。

## 2. 二段 authz

### 2.1 pre-filter（速度のための絞り込み・権限の正本ではない）

- 索引の各チャンクに構造タグ `data_table:<tenant>|<table_id>`（名前空間化のまま）を 1 つだけ焼く。
  行の可視性・共有・ロールは焼かない（PIT-64 と同じ構え）。
- 検索時に `ListObjects(user, viewer, data_table)` をクエリごとに算出し（キャッシュしない）、タグの積で絞る
  （`rag::readable_tables`）。上限 500 を超えたら tenant-only へ縮退して post-filter 全依存にする
  （PIT-1 と同じ規則）。ファイルと混在検索するときは folder/file の `ListObjects` と並列に投げ、タグ集合の和で絞る。
- テーブル共有の付与・剥奪で索引を書き換えない（grant は次のクエリで即反映・PIT-3 と同じ性質）。

### 2.2 post-filter（権限の正本・テーブルごとにクエリ 1 本）

入口は `DataStore::rag_visible_records(ctx, table_id, candidates)`。`crates/data` の読取チョークポイントを通る。

```sql
SELECT r.id, r.rev,
       coalesce((SELECT jsonb_object_agg(k.k, r.data -> k.k)
                 FROM unnest($keys::text[]) AS k(k) WHERE r.data ? k.k), '{}'::jsonb)
FROM data_record r
WHERE r.tenant_id = $1 AND r.table_id = $2 AND r.tenant_int = $3 AND r.table_int = $4
  AND (<行述語 OR 個別共有>)          -- REST の読取と同じ compile_read_predicate（v1 の意味論）
  AND r.id = ANY($ids::uuid[])
```

| 階層 | どこで効くか | OpenFGA |
|---|---|---|
| ① テーブル ReBAC | `open_for_read`（テーブル viewer を BatchCheck に束ねる・org/tenant 境界込み） | 既存タプルの check のみ |
| ② 行述語 ABAC | 上のクエリの `WHERE`（`compile_read_predicate`・値はバインド・PIT-21） | `has_role` の材料解決のみ |
| ③ フィールドマスク | 投影に入れられない（§4）。実行時にも閲覧者のマスク列と投影列の交差を再確認し、交差があれば全件を落とす | `readable_by` の材料解決のみ |
| ④ 行個別共有 | 行述語と同じ合成（`r.id = ANY(<共有 id>)`） | 既存の `data_record` スパースタプルの読取のみ |

- **OpenFGA のタプルを 1 本も増やさない**: 行を FGA オブジェクトにしない（PIT-7 の chunk と同じ判断）。
  行の可視性は SQL 述語なので、行数に比例するタプルは要らない。PoC は索引と検索の間の書込回数が 0 であることを
  計数ラッパで確かめている。
- **結果の本文は現在の値から作り直す**: 同じクエリが投影列だけ（`$keys` はバインド・SQL テキストに埋め込まない）を
  返すので、レコードの hydrate はこの 1 本で済む。索引の本文が古くても、返す本文は常に現在の可視な値。
- **失敗時の振る舞い**: テーブル viewer を持たない・テーブルが無い・オプトインしていない → そのテーブルの候補を
  全部落とす（エラーにしない。複数テーブルを跨ぐ検索で 1 テーブルの剥奪が検索全体を失敗させない）。
  材料解決の失敗（`MAX_ROLE_SET` 超過など）は REST と同じくエラーで返し、検索ごと失敗させる（fail-closed）。
- **不可視と不存在を区別しない**（存在オラクルを作らない・PIT-21）。
- **org 境界**: テーブル行の読込が `org = ctx.org` で絞る（PIT-45 の単一定義に従う）。
- **材料はリクエスト内で使い捨てる**（PIT-18）。`MAX_SHARED_IDS` の切り詰めは可視が減る方向（REST と同じ）。

### 2.3 片方が壊れても権限を守るか

- pre-filter が壊れた（タグの汚染・縮退）→ post-filter がテーブル viewer から評価し直すので、見えない行は出ない
  （PoC: `row_level_permissions_apply_to_record_search` の poison ケース）。
- post-filter の行述語が壊れた → pre-filter はテーブル単位なので、そのテーブルの viewer にはテーブル全行が見え得る。
  これはファイル検索で「file check が壊れると folder viewer に配下全ファイルが見える」のと同じ構造で、
  行の層は post-filter が唯一の防壁である。だからこそ post-filter は REST の読取と**同一の述語コンパイラ**を使い、
  `policy_threat_it.rs` の脅威モデルテストがそのまま検索経路の保証になるようにした（独自の評価器を持たない）。

## 3. 候補と検索結果の多型化（file 前提を崩す）

現状の検索は「候補 = file のチャンク（`ScoredChunk { chunk_id, node_id }`）」「結果 = `SearchResult { file_id, file_name, folder_id, page, … }`」を
前提にしている。レコードを足すために次の型を入れた（`crates/rag/src/record_source.rs`・`search_types.rs`）。

```rust
pub enum SourceRef {
    File { node_id: Uuid },
    Record { table_id: Uuid, record_id: Uuid },
}
pub struct SourceCandidate { chunk_id, source: SourceRef, score, generation: Option<String> }

#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SearchHit {
    File(SearchResult),            // 既存の応答形をそのまま中身にする
    Record(RecordSearchResult),    // { chunk_id, table_id, record_id, rev, title, content, score }
}
```

本番化の移行（ポストアルファ）:

1. **取得段**: `VectorStore::search` / `FulltextIndex::search` が `SourceCandidate` を返す形に足す
   （既存メソッドは残し、新メソッドは既定実装で file だけを返す＝加算的変更）。Qdrant の payload に
   `source_kind`（`file` / `record`）・`table_id`・`record_id`・`generation` を足し、`source_kind` に payload index を張る。
   Tantivy は**レコード用に別の index-per-tenant を持つ**（既存 index のスキーマを変えると全テナントの index が
   `SchemaError` で止まる・PIT-45 の記録と同じ理由）。
2. **融合・バックフィル**: RRF の鍵は `chunk_id` のまま。`exclude` も `chunk_id` で効く。
3. **post-filter**: `post_filter_by_file` を `post_filter_sources` に置き換える（file の判定は同じ関数
   `file_decisions` を共有しているので挙動は変わらない）。`SearchDebug` に `authz_denied_records`・`stale_chunks` を足す
   （デバッグ表示が管理者限定である点は file と同じ・オラクルになり得る）。
4. **hydrate**: file は従来どおり `rag_chunk` ＋ node JOIN。レコードは post-filter が返した投影をそのまま使う
   （`rag_chunk` にレコードの行を作らない）。
5. **rerank**: 認可済みの候補（file・レコード混在）の本文で rerank する。
6. **応答**: `SearchResponse` に `hits: Vec<SearchHit>` を足し、`results`（file のみ）は互換のため残して非推奨にする。
   型は Rust → utoipa → TS の codegen のまま（手書きの対応物を作らない）。チャットの引用（`Citation`）にも
   出所の種別を足し、引用パネルはレコードならレコード詳細へ飛ぶ。
7. **引用監査**: `rag.search` の監査に `(table_id, record_id) → allow` を足す。**`record_int` は載せない**（PIT-75）。

## 4. フィールドマスクの穴を fail-closed に倒す

チャンクは書込時に静的に作るので、投影にマスク対象列を入れると、**行が見えるユーザーにはマスク列の値まで回答・
引用に流れる**（全社員に見える人事レコードの給与列を隠しても RAG 経由で出る）。v1 は 3 段で塞ぐ。

1. **スキーマ検証**: 投影に `field_policy` の対象列を含めると 422。後から `field_policy` を付ける改訂も同じ検証を
   通るので、投影している列をマスクするには**同じ改訂で投影から外す**必要がある。
2. **世代**（`generation`）: 次の材料の sha256（先頭 128 bit）。

   | 材料 | 入れる理由 |
   |---|---|
   | 投影形式の版（`PROJECTION_FORMAT`） | 本文の作り方を変えたら全テーブルを作り直す |
   | 投影列（内部 id・表示名・順序） | 本文が変わる（表示名は本文のラベルに出る） |
   | `field_policy` 全体 | マスクの変更を索引に残さない。投影列以外への追加・変更でも変える（保守的。改訂は稀で、再構築のコストより「マスクの変更が索引に残らない」ことを取る） |

   行述語・テーブル ReBAC・個別共有は**入れない**（post-filter が毎回評価するので索引に焼き込まれていない。
   入れると共有の付け外しのたびに全件を作り直すことになる）。

   チャンクは作られた時点の世代を持ち、post-filter は**テーブルの現在の世代と一致しない候補を落とす**。
   したがって「マスク列を投影から外す改訂」の直後、索引にまだ給与入りの古いチャンクが残っていても、
   それが検索結果に出ることはない（PoC: `adding_field_policy_invalidates_and_rebuilds_index`）。
3. **実行時の再確認**: post-filter は閲覧者に対するマスク列と投影列の交差を確かめ、交差があれば（＝1 が壊れていたら）
   そのテーブルの候補を全部落としてエラーログを出す。

「候補取得後に閲覧者本人のフィールドポリシーで再投影する」方式（マスク列を投影に許す）は、チャンク本文とベクタの
生成を閲覧者ごとに分ける必要があるので別設計（data-platform §15）。

## 5. 増分索引（15.6 の outbox に相乗り）

`RagIndexWorker`（`crates/data/src/rag/indexer.rs`）が**独立の台帳 consumer**（既定 `data-rag-index`）として
outbox の `data.record.changed` を消費する。リアルタイム配信（`data-stream`）・統計（`data-stats`）とは配送を
別に記録するので、互いの消費を消さない。

```text
書込 Tx ──outbox(data.record.changed)──▶ RagIndexWorker（台帳 consumer: data-rag-index）
                                            │ テーブルごとに: 現在の世代 vs 索引の世代
                                            ├─ 一致 → 触れたレコードを現在の行から投影 → upsert / remove
                                            ├─ 不一致（初回・field_policy / 投影の変更）→ reset_table → 全件を投影し直す
                                            └─ 削除済み・オプトアウト → reset_table(None)
                                            ▼
                                   RecordChunkSink（実装は rag 側: 埋め込み → Qdrant ＋ Tantivy）
```

- **現在の行から作る**: イベントの payload の値は使わず、消費時点の行の投影列だけを読み直す（バインドした
  保存形キーで `jsonb_object_agg`）。順序の入れ替わり・再配信で古い値が勝たない。行が無ければチャンクを消す。
- **可視性を焼かない**: 索引は全行を持つ（行述語を掛けずに読む）。可視性は検索時の post-filter が毎回評価する
  ので、行述語・共有・ロールの変更では索引を書き換えない。
- **schema / refresh / derived のイベント**は世代の照合だけ行う（派生列は投影できないので本文は変わらない）。
- **冪等・at-least-once**: sink は `record_id` 単位の upsert / remove。反映と `mark_delivered` は同じ Tx で、
  sink が失敗すれば Tx ごと巻き戻って次の周期で再配信される。並行する消費は台帳 consumer の採番行のロックで直列化する。
- **初回登録**は既存のバックログを飛ばす（`register_consumer` の fast-forward）。オプトイン済みのテーブルは次の変更で
  世代不一致を検出して全件を作り直す（オプトインそのものがスキーマ改訂＝ schema イベントなので、通常はそこで作られる）。
- `record_int` は再構築の keyset の位置にだけ使い、sink へは渡さない（PIT-75）。

`RecordChunkSink`（data 側のトレイト・data は rag に依存しない）:

| メソッド | 役割 |
|---|---|
| `generation(tenant, table)` | 索引済みの世代 |
| `reset_table(tenant, table, generation)` | 全チャンクの破棄と世代の付け替え（`None` = 索引から外す） |
| `upsert(chunks)` | `record_id` 単位の upsert（古い `rev` で上書きしない） |
| `remove(tenant, table, ids)` | 行の削除 |

本番の sink（ポストアルファ）: `chunk_id = uuid5(table_id, record_id)`（`rev` を入れない＝ upsert で上書き）、
埋め込みは既存の `EmbeddingProvider`、Qdrant の payload は `tenant_id`（実装が無条件 AND）・
`authz_tags = [data_table タグ]`・`source_kind = record`・`table_id`・`record_id`・`generation`。世代は
テーブルごとの小さな台帳表（`rag_record_index(tenant_id, table_id, generation)`）に持つ。

## 6. 脅威モデル（PIT の継承）

| 観点 | 対処 |
|---|---|
| PIT-1/2 可読集合の上限・post-filter で top-k が欠ける | ファイルと同じ（上限 500 で tenant-only へ縮退・over-fetch・バックフィル） |
| PIT-3 grant の遅延 | タグは構造タグのみ。可読集合はクエリごとに算出 |
| PIT-7 タプル爆発 | 行を FGA オブジェクトにしない。post-filter は SQL 述語 |
| PIT-11 剥奪の即時反映 | テーブル viewer は `open_for_read` の check、行・共有は毎回の材料解決（キャッシュしない） |
| PIT-17 集計からの個人特定 | 検索は集計を返さない。件数はデバッグ表示のみ（管理者限定） |
| PIT-18 材料の量 | REST と同じ上限（`MAX_ROLE_SET` 超過は fail-closed・共有の切り詰めは可視減方向） |
| PIT-19 マスク列での推測 | 投影に入れられない（検索語でマスク列の値を当てる経路が無い）。世代＋実行時再確認 |
| PIT-20/71 参照越しの素通り・材料化の残留 | 派生列・参照列は投影できない |
| PIT-21 WHERE 注入だけでは不十分 | 不可視と不存在を区別しない。値・保存形キーはバインド。述語は REST と同一のコンパイラ |
| PIT-45 org 境界 | テーブル行の読込が `org = ctx.org` で絞る |
| PIT-67 巨大チャンク | 1 チャンク 4,000 文字で切り詰め |
| PIT-73 可視→不可視 | 索引は可視性を持たない。post-filter が現在の行で評価する |
| PIT-75 内部キー | `record_int` / `table_int` は sink・結果・監査に出さない |

**古いチャンクの本文で当たる件**: 同じ世代のまま値だけが更新され、索引がまだ追いついていない間は、古い本文の語で
候補が当たり得る。返す本文は post-filter が作り直した現在の値なので古い値は出ないが、「このレコードは以前その語を
含んでいた」ことは順位から推測できる。投影列はマスク対象でないので、同じ閲覧者はリビジョン履歴から旧値を読める
（新しい情報は漏れない）。マスクの変更は世代で落とすので、この推測はマスク列には及ばない。

## 7. PoC の範囲

| 項目 | PoC | 本番に要るもの |
|---|---|---|
| オプトイン・投影の検証（422） | ✅ `TableSchema.rag`・`validate_projection` | UI（テーブル設定の「検索対象にする」） |
| 世代・破棄と再構築 | ✅ 消費 Tx 内で同期に再構築 | jobq の世代付きジョブへ（索引バックフィルと同じ形）・進捗の可観測化 |
| post-filter（クエリ 1 本） | ✅ `rag_visible_records`・`post_filter_sources` | `SearchService` への組み込み・`SearchDebug` の項目追加 |
| pre-filter | ✅ `readable_tables`・`record_authz_tag` | folder/file と並列の `ListObjects`・タグ集合の和 |
| 取得段 | ❌ in-memory の偽物（部分一致） | Qdrant・Tantivy の sink 実装と `SourceCandidate` を返す検索 |
| 結果の多型化 | ✅ 型（`SearchHit` / `RecordSearchResult`） | API への公開（codegen）・チャットの引用・監査 |
| 配線 | テスト内のアダプタ（`DataVisibility`） | `crates/api` の配線層（rag と data の両方に依存する場所）に `RecordVisibility` の実装を置く |

PoC の結合テスト（`crates/rag/tests/record_search_poc_it.rs`・実 PG ＋実 OpenFGA）:

- `row_level_permissions_apply_to_record_search` — 申請者本人 or 経理ロールの行述語で、alice は自分の行だけ、bob は別の行、
  経理の carol は全行。テーブル viewer の無い dave は pre-filter で 0 件、pre-filter を汚染しても post-filter で 0 件。
  個別共有で増え、共有解除・テーブル viewer の剥奪で次の検索から消える。別テナントは 0 件。索引と検索の間に
  OpenFGA への書込が 0 回。更新・削除が outbox 経由で索引に追従し、本文は現在の値。
- `masked_columns_cannot_be_projected` — 作成・改訂のどちらでも、投影にマスク対象列を含めると `DataError::Invalid`。
- `adding_field_policy_invalidates_and_rebuilds_index` — `field_policy` の後付けで世代が変わり、再構築前の古いチャンク
  （給与入り）は世代不一致で落ち、再構築後は給与が索引から消える。投影列以外への追加でも作り直す。オプトアウトで索引から外れる。

単体テスト: `data::rag::projection::tests`（検証・世代・本文）、`rag::record_source::tests`（不可視・世代不一致の候補を落とす・
レコードだけの post-filter は OpenFGA を叩かない）。

## 8. 残作業（ポストアルファ）

1. rag 側の `RecordChunkSink` 実装（埋め込み・Qdrant payload・レコード用 Tantivy index・世代の台帳表）
2. `VectorStore` / `FulltextIndex` に `SourceCandidate` を返す検索を加算的に追加し、`SearchService` を `post_filter_sources` へ切替
3. `crates/api` の配線（`RecordVisibility` の実装・`RagIndexWorker` の起動・`SearchResponse.hits` の公開と TS 型）
4. 再構築を jobq の世代付きジョブへ移す（消費 Tx を短く・大テーブルの再構築の進捗）
5. チャットの `doc_search` / 引用パネル / 監査のレコード対応
6. テーブル設定 UI（オプトインと投影の指定・422 の表示）
7. 閲覧者別の再投影（マスク列を投影に許す方式）は別設計（data-platform §15）
