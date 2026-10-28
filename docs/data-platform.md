# 構造化データ基盤 v2（Teable / Microsoft Lists 級）

> **本書は `crates/data`（構造化データサービス）の設計正本**。Phase 9 で実装した v1（[design.md §4.10](./design.md)）を
> 業務アプリ基盤として通用する水準へ引き上げる再設計を定める。実装順は [roadmap/phase-15.md](./roadmap/phase-15.md)。
>
> 関連正本: [design.md](./design.md)（全体構成・§4.1 テナンシー・§4.10 ミニアプリ基盤）/
> [requirements.md](./requirements.md)（FR-11・NFR-8）/ [miniapp-platform.md](./miniapp-platform.md)（ワークフロー・script・skill）/
> [design-caveats.md](./design-caveats.md)（PIT-17〜21＝v1 の行 authz 脅威モデル・**PIT-69〜76**＝v2 が持ち込む落とし穴）
>
> 分冊: [data/query-optimizer.md](./data/query-optimizer.md)（自前クエリオプティマイザ＝統計・コストモデル・適応実行・
> IR の意味論。§3.5・§5・§14・§16.2 の詳細）

---

## 1. 目的とスケール目標

### 1.1 なぜ再設計するのか

v1（Phase 9 Task 9.2〜9.15）は**認可モデルとしては完成度が高い**。テーブル ReBAC → 行述語 ABAC →
フィールドマスク → 行個別共有の 4 階層、集計のスモールセル抑制、lookup への参照先ポリシー透過適用、
リビジョン、FSM ガードまで揃っている。**捨てるべきものは無い。**

一方で、次の 2 点が業務アプリ基盤として成立しない。

**(a) 物理設計がフルプールで破綻する。** v1 は索引宣言フィールドごとに `data_record` 上へ
partial 式インデックスを 1 本ずつ張る（`crates/data/src/index.rs`）。索引本数は
**テナント数 × テーブル数 × 索引フィールド数**で増え、それが単一 relation にぶら下がる。
PostgreSQL は 1 行の INSERT で当該 relation の**全索引を open し、partial 述語を全件評価**する
（`ExecOpenIndices` / `ExecInsertIndexTuples`）。プラン生成も `get_relation_info` が全索引記述子を読む。
**顧客 20〜50 社で目に見えて劣化し、数百社で停止する**。SaaS のデータプレーンはフルプール
（design §4.1・SAAS.5 達成済み）なので、これはリリースブロッカーである。

**(b) 機能が Lists/Teable に遠く及ばない。** 単一等値フィルタ・単一ソート・OFFSET ページング
（上限 10,000）・日本語列名不可・双方向リンク無し・ロールアップ無し・数式は `sum`/`concat` のみ・
ビュー定義はサーバ非解釈の不透明 JSON・リアルタイム配信無し・フロント UI 無し。

本書は (a) を**物理層の作り直し**で、(b) を**論理層の拡張**で解く。**4 階層 authz とその脅威モデル
（PIT-17〜21）は 1 系統のまま維持する**——ここを二重化しないことが最上位の制約である。

### 1.2 スケール目標

| 次元 | 一次目標 | 備考 |
|---|---|---|
| プール全体のテナント数 | **10²〜10³ 社** | 単一 Postgres プールで充足（§12） |
| 1 テナントのテーブル数 | 10²〜10³ | ミニアプリ × テーブルの積 |
| **プール全体のテーブル数** | **10⁴〜10⁶** | ← v1 の索引方式が破綻する軸 |
| 1 テーブルの行数 | 10⁶〜10⁷ | |
| プール全体の行数 | 10⁸ | |
| テーブルあたり索引フィールド | 5〜20 | |
| 1 テーブルの同時グリッド閲覧者 | 10²〜10³ | |
| グリッド初回描画 p95 | < 300 ms | 可視 100 行＋列メタ |
| スクロール 1 ページ（100 行）p95 | < 100 ms | keyset |

> **設計の合否判定**: 「索引・テーブルなどの**物理オブジェクト数がテナント数・テーブル数に比例しないこと**」。
> これは NFR-8 がフルプールに課す制約そのものであり、本書の全判断はここに従う。

### 1.3 スコープ

- **v1 スコープ**: §3〜§11（物理設計・識別子・クエリ IR・authz・リンク/数式・ビュー・リアルタイム・各面統合）
- **v1 スコープ外（将来項として §15 に記録）**: フォームビューの未認証提出・添付ファイル欄の本体化・
  レコードコメント・通知センター・条件付き書式の高度版・BI ダッシュボード

---

## 2. 前例調査と採用判断

同じ制約（**マルチテナント共有ストア × テナントごとに異なるスキーマ × 大量のテーブル**）に直面した
実装が何を選んだかを調べた。結論は明快で、**この制約下で成功した実装はすべて「pivot 索引テーブル」を選んでいる**。

| 実装 | データ格納 | 索引方式 | 物理オブジェクト数 | 採否 |
|---|---|---|---|---|
| **Salesforce (Force.com)** | `MT_Data` の汎用 flex columns `Value0..Value500`（全て可変長文字列） | **`MT_Indexes` pivot テーブル**に型付き列 `StringValue` / `NumValue` / `DateValue` を持ち、索引宣言フィールドの値を同期コピー。OrgID でネイティブパーティション | **定数** | ✅ **採用（本方式の原型）** |
| **SharePoint / Lists** | `AllUserData` の型別汎用列（`nvarchar1..`, `int1..` 等） | **`NameValuePair` テーブル**に索引列だけを別途保持し、ビュー要求はここを引いてから本体へ | **定数** | ✅ **採用（同型）** |
| **Teable** | テーブルごとに**実 Postgres テーブル**（`dbFieldName` で物理列名を分離） | 本物の btree。100 万行で複雑クエリ ~200ms | テナント×テーブル×列に比例 | ❌ ランタイム DDL 前提。フルプールで catalog が破綻 |
| **Grist** | **ドキュメントごとに SQLite ファイル**（S3 に保存し Doc Worker がローカル取得） | ファイル内の本物の索引 | ファイル数＝ドキュメント数 | ❌ Doc Worker への所在管理層（Redis で割当追跡）が別途必要。§2.2 |
| **Notion** | ブロック単位の行（Postgres・後にテナント単位シャーディング） | 汎用 | 定数 | 参考（§12 のシャード戦略） |
| **wp_postmeta 型 EAV** | `(post_id, meta_key, meta_value)`・**型なし** | `meta_value` は longtext で索引が効かない | 定数 | ❌ **反面教師**。型付き列がないと索引もソートも成立しない |

### 2.1 Salesforce / SharePoint 方式を採る理由

両者は独立に同じ答えに辿り着いている。**本体は「型が緩い汎用格納」、索引は「型付きの別テーブル」**という分離である。

- 索引の**物理本数がテナント数・テーブル数から完全に独立**する（唯一この性質を持つ）
- 索引テーブルは「テーブル → 列 → 値」の複合キーで並ぶため、**ソートが索引スキャンそのものになる**
  （keyset ページングが自然に載る）
- **索引の付け外しが DDL ではなく行の追記/削除**になる。オンラインでバックフィルでき、中断・再開・進捗観測が可能
  （§10 の自動昇格が現実的になるのはこの性質による）
- shiki の v1 は既に「ランタイム DDL なし」を不変条件に掲げているが、**実際には `CREATE INDEX` を打っていた**。
  本方式は DDL をマイグレーション以外で完全にゼロにし、掲げた不変条件を初めて文字通りにする

### 2.2 Teable / Grist を採らない理由

- **Teable（実テーブル方式）**: 索引爆発は解決するが、テーブル作成・列追加が DDL になる。
  プール全体で 10⁴〜10⁶ テーブルは Postgres の catalog・`pg_dump`・autovacuum の実務的限界を超える。
  列追加のたびに大テーブルへ `ALTER TABLE` が走る運用も受け入れがたい。
- **Grist（SQLite per document）**: 隔離とファイル単位バックアップは魅力的だが、
  「どのドキュメントがどの Worker にあるか」を管理する層（Grist は Redis ＋ Doc Worker、
  Cloudflare は Durable Objects、Turso は sqld）が別途必要になる。**shiki-server はステートレス前提**であり、
  これを崩すのはテーブル基盤とは独立した分散システムの構築になる。加えて SQLite には
  **正確な小数型がなく**（金額を扱う業務アプリ基盤として受け入れがたい）、
  DB をまたぐトランザクション（outbox・監査チェーン）と authz 実装の二重化という代償がある。
  → 詳細な比較と、将来オプションとしての残し方は §15。

### 2.3 wp_postmeta 型 EAV との違い（重要）

「pivot 索引テーブル」を素朴にやると wp_postmeta の失敗（型なし EAV）になる。本設計が回避する点:

1. **型ごとにテーブルを分ける**（text / numeric / link）。数値は `numeric` で持ち、正確な小数演算とレンジ検索が効く
2. **スキーマが正**。どの列がどの型でどのテーブルに載るかはサーバのスキーマ定義が決め、実行時に推測しない
3. **索引宣言していない列は載せない**（全列 EAV にしない）。載っていない列では検索できないことを API で明示的に 403 にする
4. **列は整数 ID**（文字列 `meta_key` ではない）。索引エントリが narrow に保たれる

---

## 3. 物理設計

### 3.1 全体構造

```text
┌──────────────────────────────────────────────────────────┐
│ ① 本体  data_record                                       │
│    レコードの中身（JSONB・キーは "f{field_id}"）           │
│    「rec_A の全項目」に答える。PK 直接アクセスのみ        │
├──────────────────────────────────────────────────────────┤
│ ② 索引  data_index_text / data_index_num / data_link      │
│         data_unique / data_index_search                   │
│    「どの値がどのレコードにあるか」だけを型付きで持つ     │
│    「状態=承認待ちを金額順に20件」に答える                │
└──────────────────────────────────────────────────────────┘
        探すのは②、中身を取るのは①（カード目録と書架）
```

### 3.2 内部整数キー

索引テーブルは 1 レコードにつき複数行を持つため、キーの幅がそのまま容量に効く。

| 論理キー | 内部キー | 型 | 採番 |
|---|---|---|---|
| `tenant_id text` | `tenant_int` | `int4` | tenant レジストリ（`crates/storage/src/tenant.rs`）で採番・不変・再利用しない |
| `table_id uuid` | `table_int` | `int8` | `data_table` 作成時にグローバル連番（**採番サービス経由**・列 default にしない・[shard-safety.md](./data/shard-safety.md)）。**パーティションキー** |
| `field.name text` | `field_id` | `int2` | テーブル内で採番・不変・再利用しない。`0..15` はシステム擬似列に予約 |
| `record.id uuid` | `record_int` | `int8` | **プール共通のシーケンス**（`data_record_int_seq`）で採番・不変・再利用しない。**索引テーブルとリンクは全てこちらで持つ** |

> **`record_int` を入れる理由（2026-10 決定）**: 索引テーブルは 1 レコードにつき複数行を持ち、`record_id` は
> 各行の PK と値索引の両方に入る。uuid（16B）を int8（8B）にすると索引行が約 7% 縮む。加えて、
> 「見える行の集合」「OR の和集合」「交差」を **Rust 側でビットマップ（Roaring の 64 ビット版）として計算できる**
> ようになる（[query-optimizer.md](./data/query-optimizer.md) §5.1・§6.4）。kintone（`row_id`）も
> SharePoint（リスト項目の ID）も内部は整数キーである。
>
> - **採番はプール共通のシーケンス**にする。テーブルごとの連番は採番行に挿入が集中して書込が詰まるため採らない。
>   テーブル内では疎になるが、挿入順に単調増加し、Roaring は疎な集合も扱える
> - **API・SSE・監査・OpenFGA のタプルは従来どおり `uuid`**。`record_int` は**外へ出さない**。プール共通の連番は
>   他テナントの作成ペースを推測させるサイドチャネルになるため（PIT-75）。keyset カーソルは `record_int` を
>   含むので、**サーバ鍵の AEAD で暗号化した不透明トークン**にする（base64 だけでは中身が読める）
> - 個別共有（OpenFGA の `data_record` タプル）は uuid で返るので、**材料解決時に 1 本のクエリで `record_int` へ
>   引き直す**（上限 `MAX_SHARED_IDS` 件・リクエストスコープ）
> - 移行は JSONB キーの書換えと同じ Phase 15.1 で行う（索引行のキーを後から差し替えると二重の書換えになる）
>
> **`field_id` の値域**: PostgreSQL に unsigned 整数は無く `int2` は `-32768..32767`。一方 `FieldDef.id` は
> `u16`（`0..65535`）なので**そのままでは 32768 以上が保存できない**。索引エントリの幅を優先して
> `int2` を維持し、**`FieldId` の上限を `32_767` とする**（`MAX_FIELD_ID = 32_767`・採番時とスキーマ検証で強制）。
> 1 テーブルに 3 万列は非現実的なので実用上の制約にならない。負値は採番しない。

**システム擬似列（`field_id` 予約枠）**——全レコードに必ず 1 行存在することが、null ソートと
初期グリッドの駆動索引として効く（§3.5）。

| `field_id` | 意味 | 格納先 |
|---|---|---|
| `0` | `owner`（作成者 principal id） | `data_index_text` |
| `1` | `created_at`（固定幅 ISO-8601 UTC） | `data_index_text` |
| `2` | `updated_at`（同上） | `data_index_text` |
| `3..15` | 予約（将来のシステム列） | – |
| `16..` | ユーザー定義フィールド | 型に応じて |

> **`tenant_int` を索引テーブルにも持つ理由**: `table_int` はグローバル一意なので理論上は冗長だが、
> ①「全行 tenant スコープ・全クエリ tenant 条件付き」という day-1 不変条件（#91）を機械的に検査可能に保つ
> ②`purge_tenant` が結合なしで走る、の 2 点のため 4 バイトを払う。**冗長性は意図的**。

### 3.3 テーブル定義

```sql
-- 内部キーの採番（§3.2）。プール共通・キャッシュ付きで採番の競合を避ける。
create sequence data_record_int_seq cache 100;

-- ① 本体。形は v1 から変えない（JSONB のまま）。キーだけ "f{field_id}" 化する。
create table data_record (
    tenant_int int      not null,
    table_int  bigint   not null,
    id         uuid     not null,   -- 外部向けの ID（API・SSE・監査・OpenFGA）
    record_int bigint   not null default nextval('data_record_int_seq'),  -- 内部キー（§3.2）
    tenant_id  text     not null,   -- 外部の tenant 識別子（v1 から残す・FK と purge 用）
    table_id   uuid     not null,   -- 外部のテーブル ID（同上）
    org        text     not null,
    data       jsonb    not null,   -- {"f16": "...", "f17": 12000}
    rev        bigint   not null default 1,
    owner      text     not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    primary key (tenant_int, table_int, id),
    foreign key (tenant_id, table_id) references data_table (tenant_id, id)
        on update cascade on delete cascade deferrable initially immediate
) partition by hash (table_int);    -- 64 分割（サイズ分割・autovacuum 局所化）
-- Phase 15 実装で変更: v1 の tenant_id / table_id と data_table への FK を残した（テーブル削除の
-- cascade と、外部 ID での照合をそのまま使うため・migrations/0085）。物理列順は固定幅の列を幅の降順に並べる
-- （アラインメントのパディングで record_int の節約が消えないように・data/internal-keys-benchmark.md）。
-- 索引テーブルから本体を引く経路（ページの本体取得はこちら）
create unique index on data_record (tenant_int, table_int, record_int);

-- ② text 系索引（text / select / multi_select / date / datetime /
--    user_ref / role_ref / file_ref / status ＋ システム擬似列 field_id 0..2）
--    val は「索引キー」であって値の正本ではない（正本は data_record.data）。
--    btree キー上限（1 ページの 1/3 ≒ 2704B）を超えないよう MAX_INDEX_KEY_BYTES で切り詰める。
create table data_index_text (
    tenant_int int      not null,
    table_int  bigint   not null,
    field_id   smallint not null,
    record_int bigint   not null,           -- §3.2 の内部キー
    ord        smallint not null default 0,  -- multi_select の要素番号
    val        text     not null collate "C",-- 先頭 MAX_INDEX_KEY_BYTES バイトのプレフィクス
    truncated  boolean  not null default false, -- true なら等値比較は本体で再確認する
    primary key (tenant_int, table_int, field_id, record_int, ord)
) partition by hash (table_int);
create index on data_index_text (tenant_int, table_int, field_id, val, record_int);

-- ③ 数値索引（number）。numeric = 正確な小数。
create table data_index_num (
    tenant_int int not null, table_int bigint not null, field_id smallint not null,
    record_int bigint not null, ord smallint not null default 0,
    val numeric not null,
    primary key (tenant_int, table_int, field_id, record_int, ord)
) partition by hash (table_int);
create index on data_index_num (tenant_int, table_int, field_id, val, record_int);

-- ④ リンク（record_ref）。1 エッジにつき「順方向」「逆方向」の 2 行を書く（下の注を参照）。
--    どちらの行も table_int = 自分が所属する側のテーブルなので、両方向でパーティション枝刈りが効く。
create table data_link (
    tenant_int int not null,
    table_int  bigint not null,       -- この行が所属する側のテーブル（＝パーティションキー）
    field_id   smallint not null,     -- 順方向はリンク列、逆方向は対称列の field_id
    src_record bigint not null,       -- この行が所属する側のレコード（record_int）
    ord        smallint not null default 0,
    dst_table_int bigint not null,    -- 相手側
    dst_record bigint not null,       -- 相手側のレコード（record_int）
    is_reverse bool not null default false, -- 監査・再構築用（クエリでは使わない）
    -- カーディナリティ強制用。この行の src 側が単一値しか持てないとき true。
    -- 書込側が自由に指定できてはならないため、下の FK でスキーマ宣言値に固定する。
    single_valued bool not null,
    primary key (tenant_int, table_int, field_id, src_record, ord)
) partition by hash (table_int);

-- リンク列ごとの宣言メタデータ。スキーマ改訂時にのみ書き換わる（レコード書込では触らない）。
create table data_link_field (
    tenant_int int not null, table_int bigint not null, field_id smallint not null,
    single_valued bool not null,          -- LinkDef.cardinality から決まる
    primary key (tenant_int, table_int, field_id),
    unique (tenant_int, table_int, field_id, single_valued)   -- ↓の FK 参照先
);

-- 【重要】single_valued を宣言値に固定する。書込側が false を指定しても、
-- 宣言が true なら参照先の組が存在せず FK 違反で落ちる（＝制約を迂回できない）。
alter table data_link add constraint data_link_cardinality_fk
    foreign key (tenant_int, table_int, field_id, single_valued)
    references data_link_field (tenant_int, table_int, field_id, single_valued);

-- 宣言したカーディナリティを DB で強制する（部分一意インデックス 1 本）。
-- 2 行方式なので「dst 側の一意」も逆方向行に対する同じ制約として表現できる。
create unique index on data_link (tenant_int, table_int, field_id, src_record)
    where single_valued;

-- ⑤ unique 制約。1 本の PK で全テーブル・全フィールドの一意性を賄う。
create table data_unique (
    tenant_int int not null, table_int bigint not null, field_id smallint not null,
    val_norm text not null collate "C",
    record_int bigint not null,
    primary key (tenant_int, table_int, field_id, val_norm)
) partition by hash (table_int);

-- ⑥ 部分一致検索（オプトイン列のみ）。trigram GIN を「1 本だけ」張るための隔離先。
-- gin_trgm_ops は pg_trgm の operator class、スコープ列の GIN 格納には btree_gin が要る。
-- 両方を先に有効化しないと初回マイグレーションが失敗する。
-- （マネージド PostgreSQL では拡張の利用可否が制限されることがあるため、
--   プロビジョニングの前提条件として明記する。Cloud SQL / AlloyDB はいずれも利用可能。）
create extension if not exists pg_trgm;
create extension if not exists btree_gin;
create table data_index_search (
    tenant_int int not null, table_int bigint not null, field_id smallint not null,
    record_int bigint not null, ord smallint not null default 0,
    val text not null,
    primary key (tenant_int, table_int, field_id, record_int, ord)
) partition by hash (table_int);
-- スコープ列を GIN 自体に含める（btree_gin）。含めないと 1 テナントの検索でも
-- プール全体の posting list を読み、ノイジーネイバーが p95 を壊す。
-- fastupdate = off: GIN の保留リスト（pending list）はパーティション内の全テナントの未整理の挿入を溜め、
-- 検索のたびに全件読まれる。スコープ列を索引に含めても保留リストには効かないので無効にする。
create index on data_index_search
    using gin (tenant_int, table_int, field_id, val gin_trgm_ops)
    with (fastupdate = off);
```

**リンクを 2 行で持つ理由（設計判断）**: 逆引きを「`dst_table_int` 条件の専用索引」で賄うと、
パーティションキーが `table_int` なので**逆引きだけ 64 パーティションを横断する** scatter-gather になり、
N:N の主要経路がテーブル数に比例して劣化する。そこで**1 エッジを順方向行と逆方向行の 2 行として持つ**
（隣接リストの標準形）。両方向とも自分側の `table_int` に載るので枝刈りが効く。
代償は書込 2 行と整合性だが、**同一トランザクションで書くため不整合は構造的に発生しない**
（対称列 `symmetric_field` が未定義なら逆方向行は書かない＝片方向リンク）。

**索引キー長の上限**: PostgreSQL の btree はキーがページの 1/3（約 2,704 バイト）を超えると
`index row size exceeds maximum` で **INSERT 自体が落ちる**。現行の `MAX_TEXT_LEN = 10_000`
（`crates/data/src/validate.rs`）はこれを超え得るため、索引テーブルには
**`MAX_INDEX_KEY_BYTES = 2_000` バイトのプレフィクスのみを格納**し、切り詰めた行は `truncated = true` にする。

- 等値・`StartsWith`: プレフィクスで候補を絞り、`truncated = true` の候補のみ**本体で再確認**する
- `data_unique`: 長い値は `val_norm = <先頭 2,000B> || ':' || sha256(全体)` として一意性を保つ
- 全文の部分一致は `data_index_search`（trigram）が担当する（btree キー長の制約を受けない）
- **材料化した派生列（§7.4）は本体に値を持たないので再確認ができない**（Phase 15 実装で追加）。比較値
  （等値・`in`・`starts_with`・範囲の端）を `MAX_INDEX_KEY_BYTES − 4` = **1,996 バイト以下に限り**、
  超える比較値は 400（`DataError::Invalid`）で拒否する。この長さ以内ならどの比較もプレフィクスだけで
  厳密に決まる（判定は比較値だけで決まり、データの有無・長さのオラクルにならない）
- **`group_by` の切り詰め値**: 実フィールドは切り詰め行だけ本体の完全値でグループ化する。材料化した派生列は
  完全値が無いので、可視かつ条件を満たした行に切り詰め行が 1 つでもあれば 400 で拒否する
  （違う値を 1 グループに混ぜた集計を返さない）

**切り詰め値でのソートと keyset の契約**（曖昧にすると重複・欠落が出るので明文化する）:

- ソートは**プレフィクス順で走査する**。同一プレフィクス群の中の順序は**完全値順を保証しない**
- ただし **keyset は `(val_prefix, record_int)` のタプル比較で進む**ため、
  **ページ間の重複・欠落は発生しない**（順序が「完全値順ではない」だけで、全順序としては決定的）。
  `record_int` が常にタイブレーカに入ることがこの保証の根拠
- カーソルに載せるのは**索引に格納されているプレフィクス値**であり、完全値ではない
  （完全値を載せると索引の走査位置と一致せず、そこで初めて欠落が起きる）
- 切り詰めが発生し得る列をソートキーに指定した場合、**API は応答に
  `sort_precision: "prefix"` を含めて呼び出し側へ明示**し、ビュー設定 UI でも警告を出す
- 完全値順が業務要件になる列は、`MAX_INDEX_KEY_BYTES` 以内に収まる正規化列
  （読み仮名列・コード列など）を別に持ち、そちらでソートする

**ページ取得の途中で行が変わったときの契約**（keyset が決定的なのは「静的な集合に対して」であり、
ライブデータではソートキーの更新・削除で行が前後に移動して重複・欠落が起きる）:

| モード | 保証 | 用途 |
|---|---|---|
| **ライブページング**（既定） | **ページ間の重複・欠落は保証しない。** ページ内は決定的 | グリッド閲覧。SSE の差分配信（§9）が同一セッションで補正するため、実用上の破綻にならない |
| **スナップショット読取** | **基準 `snapshot_seq` にカーソルを束縛**し、その時点の集合に対して重複・欠落なし | エクスポート・集計・ワークフローの一括処理など、一貫性が要件になる経路 |

- スナップショット読取は、カーソルに `snapshot_seq`（§9.2 の配信位置）を含め、
  **それ以降に発生した変更を読取結果から除外**する（`data_record.updated_at` ではなく配信位置で判定し、
  SSE と同じ時間軸に揃える）
- 保持期間を超えた `snapshot_seq` を指定されたら `410 Gone` を返し、再取得させる

> **Phase 15 実装で確定した形**（`crates/data/src/query/snapshot.rs`）:
>
> - 並びは**内部の不変な位置（`record_int`）の順だけ**（おおむね作成順だが、採番のキャッシュで厳密な作成順ではない）。
>   ソートキーは更新で動き重複・欠落の源になるので、`sort`・集計・件数（`with_count`）・結合／リンクの条件は
>   指定できない（400）。材料化した計算列の条件も 400（値は索引行にしか無く、本体の SQL では評価できない・独立レビュー R1-E）
> - ページごとに「`snapshot_seq` より後に変わった行」（ストリームの `seq > S` と、まだ採番されていない outbox）を引き、
>   現在の本体から除いたうえで、`S` 時点の値をリビジョンから復元して同じ位置に差し込む。`S` より後に作られた行は出さず、
>   `S` より後に消えた行は `S` 時点の値で出す。行述語・条件は復元した値にも同じ SQL で評価する
> - `S` より後の変更が **10 万件を超えたら 410**（数え切れない）。ストリームの保持期間（既定 10 分・§9.4）を過ぎた `S` も 410
> - 派生値（ルックアップ等）とマスクは他の読取と同じく**読取時点に閲覧者本人の権限で**計算する（`S` 時点へ巻き戻さない）
> - ライブページングの応答も `snapshot_seq`（読取時点の配信位置）を返す。SSE の `since` にこれを渡せば読取後の変更を取りこぼさない
- **ライブページングでは「途中で行が消えた／二重に出た」をクライアントが検知できるよう、
  各ページに `snapshot_seq` を添えて返す**。グリッドはこれと SSE の `delivery_seq` を突き合わせて
  自前で整合を取る（再読込を強制しない）

**索引の総本数はパーティションあたり 14 本の定数**（PK 7 本＝`data_record` と索引 6 表に 1 本ずつ、
`data_record` の `(tenant_int, table_int, record_int)` 一意索引 1 本、値索引 3 本＝`text`/`num`/`composite`、
GIN 1 本＝`search`、リンクのカーディナリティ強制用の部分一意 1 本、リンクの参照先からの逆引き 1 本＝
`data_link (tenant_int, table_int, field_id, dst_record)`。対称列のあるリンクは 2 行方式で逆向きの行を引けるが、
対称列の無い片方向リンクの参照元（材料化した派生列の再計算・派生値の再配信）は参照先から引くので、この索引で読む
（migrations/0101）。
（Phase 15 実装で変更: 当初の「10 本」は `data_record` の一意索引と、§5.3 の複合索引表 `data_index_composite`（PK・値索引）、
片方向リンクの逆引きを数えていなかった。数え方が変わっただけで、テナント数・テーブル数・列数に依存しない定数である点は同じ。
`pivot_index_it::index_count_does_not_grow_with_tables_or_tenants` が確かめる。）`data_link_field` は
**リンク列ごとに 1 行**の小さなメタデータ表で、レコード数には比例しない。
**テナントが何社増えても、テーブルが何個増えても、列が何本増えても変わらない。**
（v1 は「テナント数 × テーブル数 × 索引列数」本が単一 relation に載る。10³ 社 × 10² テーブル × 5 列で 5×10⁵ 本。）

### 3.4 値の正規化

| 型 | 格納先 | 正規化 |
|---|---|---|
| `text` / `select` | `data_index_text` | そのまま |
| `multi_select` | `data_index_text` | **要素ごとに 1 行**（`ord` 昇順）。包含判定が等値検索になる。`searchable` を宣言すると `data_index_search` にも要素ごとに 1 行（部分一致は「要素のいずれかの部分文字列」・Phase 15 で text 以外に multi_select も許した） |
| `number` | `data_index_num` | `numeric`（正確な小数） |
| `date` | `data_index_text` | `YYYY-MM-DD` 固定幅 → 辞書順＝日付順 |
| `date_time` | `data_index_text` | UTC 固定幅 ISO-8601 → 辞書順＝時刻順（v1 の正規化を踏襲） |
| `user_ref` / `role_ref` / `file_ref` | `data_index_text` | principal id / role id / node id |
| `record_ref`（リンク） | `data_link` | 参照先 `(dst_table_int, dst_record)`。多値は `ord`。対称列があれば逆方向行も同時に書く |
| `owner`（擬似列 `field_id=0`） | `data_index_text` | 全レコードに 1 行。`IsOwner` 述語を索引内で評価するため |
| `created_at` / `updated_at`（擬似列 `1` / `2`） | `data_index_text` | 全レコードに 1 行。**null を持たない全件駆動索引**として使う（§3.5） |

> **`COLLATE "C"` を使う理由**: バイト順の決定性を得るため。ICU ロケール照合は collation version が
> 上がると既存索引が壊れる（`pg_collation` バージョン不一致）リスクがあり、索引テーブルには不適。
> **代償**: 漢字の並び順は符号位置順になり、日本語として正しくない。
> → **読み仮名フィールドを別に持ち、それでソートする**のが正解（Excel/kintone も同じ運用）。
> ビューのソート設定 UI で「この列の並べ替えには読み列を使う」を指定できるようにする（§8）。

### 3.5 クエリの形

**「状態=承認待ち の申請を、金額が高い順に 20 件」**

```sql
WITH page AS (
  SELECT i.record_int, i.val
  FROM data_index_num i                                    -- 駆動＝ソートキーの索引（既に金額順）
  WHERE i.tenant_int = $1 AND i.table_int = $2 AND i.field_id = $3   -- f2 = 金額
    AND (i.val, i.record_int) < ($cursor_val, $cursor_rec)           -- keyset（DESC）
    AND EXISTS (SELECT 1 FROM data_index_text f                      -- フィルタ＝半結合
                WHERE f.tenant_int = $1 AND f.table_int = $2
                  AND f.field_id = $4 AND f.val = '承認待ち'
                  AND f.record_int = i.record_int)
    AND ( <行述語も索引テーブル上で評価。§6.2> )
  ORDER BY i.val DESC, i.record_int DESC
  LIMIT $n
)
SELECT r.* FROM data_record r
  JOIN page p ON r.tenant_int = $1 AND r.table_int = $2 AND r.record_int = p.record_int
ORDER BY ...;
```

- フィルタは `EXISTS` 半結合。複数条件は AND で連鎖
- 可視でない行は**本体に触れる前に落ちる**
- 本体取得は `(tenant_int, table_int, record_int)` の一意索引で 20 回

#### 駆動索引の選び方（null 落ちと初期表示の両方がここに掛かる）

pivot 索引は**値が無いレコードには行が無い**。ソートキーの索引を素朴に駆動表にすると、
**そのフィールドが未設定の行が結果から丸ごと消える**（`NULLS LAST` に並ぶのではなく欠落する）。
任意列でのグリッドソートは通常操作なので、これは可視行の欠落＝バグになる。

そこで駆動索引を次の規則で選ぶ。

| 状況 | 駆動 | 早期終了 |
|---|---|---|
| ソートキーが `required`（全行に値がある） | そのフィールドの索引 | ✅ 効く |
| ソートキーが任意（null あり） | **2 区間に分けて歩く**: ①ソート列の値索引（値を持つ行が値順）→ ②使い切ったら擬似列 `created_at`（=1）を主キー＝`record_int` 順に歩き「値が無い」を点検索（[query-optimizer.md §5.1](./data/query-optimizer.md)） | ✅ 効く（`NULLS LAST` の先頭ページはほぼ区間①で埋まる） |
| ソート指定なし（初期グリッド） | `updated_at`（擬似列 2）の索引 | ✅ 効く |
| 選択的フィルタがある | そのフィルタの索引を駆動にしてソートは後段 | 統計で判断（§14） |

**システム擬似列（`owner` / `created_at` / `updated_at`）は全レコードに必ず 1 行あるため、
「全件を走査できる索引」として常に使える。** これが null 落ちと「フィルタなしの初期グリッド」の
両方を解く鍵になる。

> 当初は「擬似列を駆動にしてソート値を `LEFT JOIN` し `NULLS LAST`」としていたが、これでは早期終了が効かない。
> 2 区間走査に改めた。値なしの位置は昇順・降順とも末尾に固定する（`NULLS FIRST` は受け付けない）。
> 区間 2（値なしの行）は擬似列の走査になり、値なしが疎な列では末尾ページが O(n) になる。末尾ページを
> 頻繁に使う列に限り、値なしの印の行（未設定レコードにも行を持たせる）を §10 の昇格で材料化して範囲走査にする。
> カーソルには区間番号を含める（query-optimizer.md §5.1・§6.3）。

**どの戦略を選ぶか（統計・コスト・予算付きの切替）は [query-optimizer.md](./data/query-optimizer.md) が正本**。
上表は戦略の種類を示すもので、表の行を固定的に当てはめるのではない。

### 3.6 書き込みの形

1 トランザクションで:

```text
① data_record の data を更新（変更フィールドのみ）・rev+1・updated_at
② 変更のあったフィールドの索引行のみ upsert / delete
   （どの列が変わったかは v1 の FieldPatch が既に持っている）
③ data_record_revision へ差分 1 行（追記型）
④ outbox へ変更イベント 1 行（§9）
⑤ 監査（Chain::Yes・v1 の pg_advisory_xact_lock 連鎖をそのまま維持）
```

**索引更新は差分のみ**。金額だけ変えたなら `data_index_num` の 1 行だけを触る。

### 3.7 容量と書込増幅の会計

**1 レコードあたりの索引行数** = 索引スカラー列数 ＋ 多値列の要素数合計 ＋ 1（owner）

索引行 1 行あたりの実測見積:

| 内訳 | バイト |
|---|---|
| 本体（tuple header 23 ＋ キー 30 ＋ 値 ~20 ＋ アライン） | ~100 |
| PK 索引エントリ | ~60 |
| 値索引エントリ | ~75 |
| **計** | **~235** |

（上はレコードキーを uuid とした見積もり。`record_int`（§3.2）で PK と値索引のキーが 8B ずつ縮み、**~219B** になる。）

| 前提 | 索引行数 | 概算 |
|---|---|---|
| プール総レコード 10⁸ × 索引行 8 | 8×10⁸ 行 | **~190 GB** |
| 本体 10⁸ 行（20 列 JSONB） | 10⁸ 行 | ~100 GB |

**本物の btree なら同じ索引に ~30 GB**。本方式は **5〜6 倍の容量**を索引に使う。これは pivot 方式の
本質的な代償であり、隠さずに受け入れる。判断根拠:

1. 本体自体が同オーダー（190 GB は「桁違い」ではなく「倍」）
2. 10⁸ 行は**プール全体**の極端側。10²〜10³ 社の一次目標では実ディスクは数十 GB
3. **比較対象は「容量が少ないが動かない設計」**である

**緩和策**（設計に織り込む）:

- 内部整数キー（§3.2）で 2〜3 割減
- 索引はオプトイン（既定で全列索引しない）＋ 自動昇格（§10）
- **多値列（multi_select / link）の要素数に上限**を設ける（1 レコードが数千行に膨らむのを防ぐ・→ **PIT-72**）
- パーティション単位のアーカイブ／退避

### 3.8 パーティション

`PARTITION BY HASH (table_int)` を 64 分割。クエリは常に `tenant_int = $1 AND table_int = $2` を
持つため、**必ず 1 パーティションに枝刈りされる**。

- サイズ分割と autovacuum の局所化が目的（索引本数の削減が目的ではない——それは §3.3 が既に解決している）
- 分割数はプロビジョニング時の設定値（初回マイグレーション時の GUC `shiki.data_partitions`・既定 64。
  全索引テーブルは `data_record` と同じ分割数で作る）。増やす場合は再ハッシュが要るため、初期値は余裕をもって決める
- **テーブルごと LIST パーティションは採らない**: パーティション数がテーブル数（10⁴〜10⁶）になり、
  partition descriptor と relcache が破綻する

---

## 4. フィールド識別子の 3 層分離

### 4.1 v1 の問題

`FieldDef.name` が「識別子」「表示名」「JSONB キー」「索引 DDL に埋め込む文字列」を兼ね、
`^[a-z][a-z0-9_]{0,63}$` に制限されている（`crates/data/src/model.rs`）。結果:

- **「申請者」「承認ステータス」という日本語の列名が作れない**
- 列名を変えると**全レコードの JSONB を書き換える**必要がある

### 4.2 設計

```rust
pub struct FieldDef {
    /// 内部 ID。テーブル内で採番・不変・再利用しない。
    /// JSONB キー = "f{id}"、索引テーブルの field_id。0 は owner 擬似列に予約。
    pub id: FieldId,            // u16
    /// API / shiki script / workflow / SDK が参照する安定スラッグ。^[a-z][a-z0-9_]{0,63}$・不変。
    pub key: String,
    /// 画面に出る名前。自由文（日本語可）・いつでも変更可。
    pub display_name: String,
    /// 既定の表示順（ビューが上書き可能）。
    pub order: i32,
    /// 説明（列ヘッダのツールチップ）。
    pub description: Option<String>,
    // 以下 v1 から継承
    pub field_type: FieldType,
    pub required: bool,
    pub unique: bool,
    pub indexed: bool,
    pub searchable: bool,       // 新規: 部分一致検索（data_index_search）へ載せる
    pub options: Vec<String>,
    pub ref_table: Option<Uuid>,
    pub lookup: Option<LookupDef>,
    pub computed: Option<ComputedDef>,
    /// number 型のみ: 小数桁数と範囲。表示・丸めの規則を決め、
    /// 併せて将来の物理層差し替え時の変換を機械的にする（§15.1 の保険③）。
    pub numeric_spec: Option<NumericSpec>,
    // Phase 15.4 で追加（§7）: record_ref のリンク定義・ロールアップ・数式と、
    // サーバがスキーマ保存時に決める（入力は無視する）材料化可否と値の型。
    pub link: Option<LinkDef>,
    pub rollup: Option<RollupDef>,
    pub formula: Option<FormulaExpr>,
    pub materialization: Option<Materialization>,
    pub result_type: Option<ResultType>,
}

/// `number` フィールドの精度宣言。
pub struct NumericSpec {
    /// 小数桁数（0..=9）。保存値の正規化・表示・入力検証に使う。
    pub scale: u8,
    /// scale を超える桁が来たときの扱い。
    pub on_excess_scale: ExcessScale,
    /// 許容範囲（省略時は型の上限）。**正規化後の値**に対して検証する。
    /// Phase 15 実装で変更: 型は f64（JSON 数値のまま受ける）。比較は境界値を 10 進の最短表現として
    /// 解釈して行う（`0.1` は 0.1 ちょうど）ので、2 進浮動小数の誤差は境界判定に入らない。NaN/∞ は拒否。
    pub min: Option<f64>,
    pub max: Option<f64>,
}

pub enum ExcessScale {
    /// 拒否する（既定。入力ミスを黙って変えない・400）
    Reject,
    /// 指定の丸めモードで丸める
    Round(RoundingMode),
}

/// 丸めモード（曖昧さを残さないため明示列挙）。
pub enum RoundingMode { HalfUp, HalfEven, Down, Up }
```

**正規化規則を固定する**（未定義のままだと保存値・pivot 索引・集計・将来の変換で結果がずれる）。

1. 書込時、値を **`scale` へ正規化**する。`on_excess_scale = Reject` なら
   scale を超える桁がある入力は **400**（既定。黙って値を変えない）。
   `Round(mode)` なら宣言したモードで丸める
2. **`min` / `max` の検証は正規化の後**に行う（丸めで境界を跨ぐケースの挙動を一意にする）
3. **本体・索引・集計はすべて正規化後の同一値を使う**（本体と索引で丸めが違う事故を防ぐ）

**スキーマ検証**（`NumericSpec` 自体の妥当性）:

- `field_type != Number` に `numeric_spec` が付いていたら 400
- `min > max` は 400
- `scale` が範囲外（`> 9`）は 400

（スキーマ検証・入力値の検証の拒否は構造化 400（`schema_invalid` / `value_out_of_range` 等）。HTTP ステータスの対応は §5.6。）

> **精度の限界（検証 A P3 の仕様確認）**: 値は JSON の数値で受ける。整数は i64 / u64 の範囲なら正確に保存する。
> 小数はサーバが受け取った時点で IEEE 754 倍精度に解釈されるので、17 桁を超える有効桁はサーバでは検出できない
> （`numeric_spec` を宣言した列は、本体と索引で同じ値を保てない桁を 400 にする）。17 桁以上の値を正確に扱う画面は
> 送る前に桁を検査する。

> **`scale` を宣言させる理由**は 3 つ。①金額と割合と個数で丸め方が違うので、**表示規則を
> スキーマが持つべき**（kintone / Airtable も小数桁数を列設定に持つ）②入力検証が決まる
> ③**将来 `numeric` を持たない物理層へ移す場合の変換が機械的になる**（`scale` が分かれば
> スケール済み整数へ落とせる・§15.1）。索引テーブルは Postgres 上では `numeric` のまま——
> 任意精度を捨てる必要はない。

| | 役割 | 変更 | 例 |
|---|---|---|---|
| `id` | JSONB キー・索引 field_id | **不可** | `7` → `"f7"` |
| `key` | script/workflow/SDK の参照名 | **不可** | `applicant` |
| `display_name` | 画面表示 | **自由** | `申請者` → `申請者氏名` |

**表示名の変更はスキーマ 1 箇所の書き換えで完結し、データは 1 行も動かない。**

> `key` を不変にする理由: script / IR / 保存ビューが `key` で列を参照するため、リネームを許すと
> 参照解決に別名解決層が要り、失効した参照の扱いが fail-closed にできない。表示名は自由に変えられるので
> 実運用上の不便はない。

### 4.3 移行

既存スキーマから機械的に導出する（§13 M1）。

```text
既存 name = "applicant"
  → id = 宣言順に 16 から採番（0..15 はシステム擬似列に予約・§3.2）
  → key = "applicant"（そのまま）
  → display_name = "applicant"（後からユーザーが日本語へ変更）
JSONB: {"applicant": "佐藤"} → {"f16": "佐藤"}
```

実装（Task 15.1）: テーブル単位の jobq ジョブ（`crates/data/src/key_migration.rs`）。1 テーブル = 1 トランザクションで
`data_table.key_format` を 1 → 2 へ一方向に進めるので冪等で、中断したテーブルはロールバックされて次回やり直す。
未移行テーブルへのアクセスはその場で同じ関数により移行してから処理する（旧形式を読む経路を残さない）。
外部契約は `key` で受けて `key` で返し、`"f{id}"` は保存形と SQL 生成に閉じる。v1 の入力 `name` は `key` の別名として受け、
`id` を省略したフィールドは既存の同じ `key` の `id` を引き継ぐ。次の `id` は `data_table.next_field_id` が持つ
（削除済みフィールドの `id` も再利用しないため、スキーマ JSONB の外に置く）。

---

## 5. クエリ IR

### 5.1 型

```rust
pub struct DataQuery {
    pub filter: Option<Condition>,   // AND/OR/NOT の条件木
    pub sort: Vec<SortKey>,          // 複数キー（最大 4・空なら擬似列 updated_at の降順）
    pub limit: Option<u32>,          // 1 ページの件数（既定 50・最大 200）
    pub cursor: Option<String>,      // 前ページの next_cursor（不透明トークン）  ← OFFSET は廃止
    pub select: Option<Vec<String>>, // 投影（グリッドは可視列だけ取る）
    pub group_by: Vec<String>,
    pub aggregate: Vec<MetricSpec>,  // 複数指標
    pub joins: Vec<JoinSpec>,        // 宣言済みリンク列経由の INNER JOIN（最大 3 段・§16.1）
    pub having: Option<HavingCond>,  // 集計後の条件（抑制されたグループの値は NULL＝どの比較も不成立）
    pub group_sort: Vec<GroupSortKey>, pub group_limit: Option<u32>, // 集計結果の並べ替えと件数（最大 1,000）
    pub search: Option<String>,        // 横断部分一致（searchable 列のみ・最大 256 文字）
    pub search_fields: Option<Vec<String>>, // 部分一致の対象列を絞る（マスク列は 403）
    pub with_count: bool,            // 上限付き件数（§5.4・既定 false）
    pub offset: Option<i64>,         // v1 互換で受けるだけ（0 以外は 400）
    pub snapshot: bool,              // スナップショット読取（§3.3）
}

pub enum Condition {
    All(Vec<Condition>),
    Any(Vec<Condition>),
    Not(Box<Condition>),
    Cmp(CondCmp),        // CondCmp { field: String（参照名 key）, op: CondOp, value: Option<Json> }
    Link(LinkCond),      // リンク列で辿った参照先に「見えて filter を満たす行」がある（半結合・§16.2）
}

pub enum CondOp {
    Eq, Neq, Lt, Lte, Gt, Gte, In,
    Contains, StartsWith, EndsWith,   // 文字列
    Exists, IsNull,                   // 有無
    Matches,                          // 語彙の互換のためだけに受け、data では常に 400
}
```

> **Phase 15 実装で変更**（`crates/data/src/query/ir.rs`）: `Page` 型は作らず `limit` / `cursor` を平らに持つ
> （v1 の SDK・保存済みビューの形を崩さないため。v1 の `filter: {field, value}`・単一 `sort`・単一 `aggregate`・
> `offset` も受理して v2 の形へ写す）。条件の葉は `Condition::Cmp(CondCmp)`。15.10 の SQL 前段面に合わせて
> `Link`・`joins`・`having`・`group_sort`・`group_limit`、15.3 で `search_fields`・`snapshot` を足した。
> 応答 `QueryResult` は `items` / `groups`・`next_cursor`・`count`（`{"exact": n}` か `{"at_least": n}`）・
> `sort_precision`（§3.3）・`sort_degraded`（§5.3）・`shares_truncated`・`suppressed`・`snapshot_seq`（§3.3）。
> 推定値・統計値・戦略名・予測コストのフィールドは持たない（PIT-75）。
> グリッドの HTTP 要求（`POST /data/tables/{id}/query` の `DataQueryRequest`）だけは `promote_indexes`（§10）を
> 足せる。チャット・ワークフロー・ミニアプリの受け口は `DataQuery` そのもので、このフラグを持たない。

**演算子語彙は workflow IR の `Condition`（`crates/workflow-engine/src/ir/`）に合わせる。**
そうすることで `data.query` ノードは IR 条件を**変換なしで**渡せる。
**意味論も揃えて 2 値論理とする（2026-10 決定）**（値なしに対する `Neq` は真・`Not` は補集合。Excel のフィルタと同じ）。
多値列（`Eq` は「含む」）と数値の等値（`1 = 1.0`）は、**workflow の評価器 `control/eval.rs` をこちらに合わせる**
（Task 15.7）。値なし・多値列での各演算子の定義、SQL 前段（3 値論理）からの翻訳規則、
**行述語は v1 の意味論のまま（値なしの `Ne` は偽）であること**は [query-optimizer.md §3.1・§3.3](./data/query-optimizer.md) を正とする。
`matches`（正規表現）は索引が効かず DoS 面にもなるため**採用しない**（IR 側にはあるが data では拒否）。
語彙の一致は `crates/workflow-engine/tests/cmp_op_vocab.rs` が生成型の列挙値で検査し、意味論の一致は
共通の表 `semantics_table.json`（`data::CONDITION_SEMANTICS_TABLE`）を workflow の評価器・data の参照評価器・
実 SQL（索引経路と本体経路）の 3 者にかけて固定する（`semantics_table_it.rs`・`eval_semantics_tests.rs`）。

> **`Not` の扱い**: `row_policy`（`PolicyExpr`）は意図的に `Not` を持たない（フィールドマスクとの
> 相互作用で情報が漏れるため・`crates/data/src/policy/ast.rs`）。**クエリフィルタは別物なので `Not` を許す**。
> マスク列はそもそもフィルタに出せない（§6.3）ため、否定による漏洩経路は生じない。この非対称は意図的である。

### 5.2 索引到達可能性の強制

v1 の `indexed_queryable` を継承・強化する。

| 制約 | 理由 |
|---|---|
| `filter` / `sort` / `group_by` / `aggregate` の対象は `indexed \|\| unique` 宣言済みのみ | 索引テーブルに載っていない列は引けない |
| マスク対象フィールドは上記いずれにも出せない（403） | PIT-19（表示を隠しても並べ替えで漏れる） |
| `Contains` / `EndsWith` / 横断検索（`search`）は `searchable` 宣言済みのみ（`indexed` は要らない） | trigram GIN（`data_index_search`）を要する。値索引は使わない（検証 2-B-P1-1） |
| **駆動索引が 1 つ以上決まること**（索引済みフィルタ条件／`required` なソートキー／システム擬似列のいずれか） | 全走査クエリを API から作れなくする。**システム擬似列は常に駆動になれるので「フィルタなしの初期グリッド」は成立する** |
| `statement_timeout` を data 経路に設定 | 想定外プランの保険 |

> 「条件木に索引可能な述語を最低 1 つ」という強い形にはしない。`filter = None` の初期グリッド
> （テーブルを開いた直後の 100 行表示・Phase 15.5 の要件）が常に拒否されてしまうため。
> 代わりに**駆動索引の存在**を条件とし、システム擬似列（全行に存在）を既定の駆動として認める。

**索引未宣言の列を指定されたら 403 を返しつつ、自動昇格（§10）を提案する**のが UX 上の解。

> **Phase 15 実装で変更**: **索引の無い**（`indexed` でない）列の指定は 403 ではなく **400** `field_not_indexed`。
> グリッドが `promote_indexes: true` を付けたときだけ、400 で終わらせず自動昇格する（§10）。
>
> **存在しない列と読めない（マスクされた）列は同じ 403**（2 回目の検証 2-B-P2-1 / 2-C-P2-2 で決定）:
> 15.3 の受け入れ条件「マスク列を filter / sort / group_by / aggregate / search に指定すると 403（PIT-19 継承）」を
> 守ったまま列の存在オラクルを消すため、REST・一括取込（JSON の行・CSV の見出しの参照名と表示名）・チャット・
> ワークフロー・スクリプト・ゲートウェイ・保存ビューの検証・索引の昇格・カーソル位置の通知のすべてで、
> 存在しない列とマスク列を**同一の 403**（`DataError::FieldUnavailable`・HTTP 本文
> `{status: 403, title, code: "field_unavailable", message: "指定した列は存在しないか、参照できません"}`・列名を
> 含めない）で拒否する。SQL 面（§16.1）は 422 `unknown_name` で、SQL 面の中で同一なので従来どおり。

### 5.3 ページング

（Phase 15 実装では `Page` 型を作らず、`DataQuery` の `limit` と `cursor` で表す・§5.1。）

カーソルは **全ソートキーの値 ＋ `record_int`（＋任意列ソートの区間番号）** を、サーバ鍵の AEAD で暗号化した不透明トークンにしたもの（`record_int` を外へ出さないため・§3.2）。

**単一ソートキーの場合**は `(val, record_int)` のタプル比較で継続位置へ直接飛べる。
駆動索引の並びがそのまま解の並びなので、**何ページ目でも同じコスト**。

**複数ソートキーの場合は事情が違う（正直に書く）。** pivot 索引はフィールドごとに別行なので、
`(field_a, field_b, record_int)` の複合順序を直接持つ btree は存在しない。第 1 キーが同値の
レコード群を、第 2 キー以降で並べ替える処理がどこかに要る。設計は次の 2 段構え。

1. **既定（同値群が小さい場合）**: 第 1 キーの索引で走査し、**同値群をページサイズの K 倍（既定 8 倍）まで
   先読みして、残キーでページ内ソート**する。同値群がこの範囲に収まる限り keyset は決定的に進む。
2. **同値群が上限を超える場合**: 第 1 キーの選択性が低すぎるので、**複合索引を材料化する**
   （`data_index_composite(tenant_int, table_int, sort_set_id, val_concat, record_int)` を
   §10 の自動昇格と同じ機構でオンデマンド生成する）。生成までは「このソート組み合わせは
   深いページで劣化する」ことを API が明示して返す。

> **Phase 15 実装の形**（`migrations/0089_data_index_composite.sql`・`crates/data/src/index/composite.rs`）:
>
> - ソートの組み合わせは `data_sort_set`（`spec` = `"16:a,17:d"`・`building` / `active`・バックフィルの進捗）に登録し、
>   `data_index_composite` は PK `(tenant_int, table_int, sort_set_id, record_int)` と値索引
>   `(tenant_int, table_int, sort_set_id, val)` の 2 本。`val` は**全順序（ソートキー → `record_int`）を保つ 1 列の
>   `bytea`**（順序を保つ符号化。text はプレフィクス値から作るので、カーソルの定義と一致する）
> - 組み合わせは**テーブルあたり 8 組まで**。登録は `data_table` 行の `FOR UPDATE` の下で行い、`building` の間は
>   書込が write-through し、クエリには使わない（§10 と同じ）
> - `sort_degraded: true` は、**可視行で見て**第 1 キーの同値群がページの 8 倍を超えた（＝この組み合わせの複合索引が
>   まだ使えず、深いページほど遅くなる）ときだけ返す。見えない行の分布は応答に出ない

**受け入れ条件には最悪ケース（第 1 キーが 1 値に偏ったテーブルでの複数ソート・深いページ）を含める**（§14）。

> フロントの `web/src/hooks/use-infinite-list.ts` は既に `next_cursor` 前提の実装なので噛み合う。
> v1 の OFFSET（clamp 10,000）はこのフックとインピーダンス不整合だった。

> **UI の妥協（記録）: スクロールバーで遠くへ飛べない。** グリッド（`web/src/components/data/data-grid.tsx`）は
> keyset のカーソルで先頭から順に読むので、スクロールバーの長さは「読み込み済みの行＋読み込み中の 20 行」で、
> 全件（100 万行）の長さではない。つまみを末尾へドラッグしても「次のページ」が読まれるだけで、50 万行目のような
> 遠い位置へ直接は飛べない（OFFSET を使わない以上、任意の位置の先頭行を安く求める手段が無い）。遠くの行は
> 並べ替え（逆順にすれば末尾が先頭に来る）か絞り込みで手前に持ってくる。件数は別リクエストで「◯件」または
> 「◯件以上」（§5.4）として出し、スクロールバーの長さとは独立させている。計測は
> [data/benchmarks.md](./data/benchmarks.md) §6。

### 5.4 件数

10⁷ 行で「条件に合う可視行の正確な件数」は成立しない（全行の権限判定が必要）。

```rust
pub enum CountResult {
    Exact(u64),              // 上限内
    AtLeast(u64),            // "10,000+"
}
```

`LIMIT 10001` のサブクエリで数え、10001 なら `AtLeast(10_000)`。

**件数は既定で数えない**（`DataQuery` に `with_count: bool`・既定 `false`）。件数はページとは別の 2 本目のクエリになり、
フィルタ駆動と同じく「候補を全部見る」形になるため、ページの取得を遅らせる。グリッドは件数を別リクエストで
非同期に取り、ページの描画を待たせない。kintone も件数取得（`totalCount=true`）を遅くなる要因として挙げ、
不要なら指定しないよう案内している（[cybozu.dev](https://cybozu.dev/ja/kintone/tips/best-practices/performance/complement-technical-consideration-points/)）。
Airtable / Teable / SharePoint も同種の妥協をしている（SharePoint の 5,000 件ビュー閾値が典型）。
**上限値はテナント設定で変更可能**にする（小さいテーブルしか持たない顧客には正確な件数を出せる）。
実装（Task 15.3）: `data_tenant_settings.count_cap`（1〜10,000,000・未設定は既定 10,000）。変更は組織管理者だけが
`/org-admin/data/settings` で行い、監査に残す（`query_ops_it::count_cap_is_a_tenant_setting`）。

### 5.5 集計

v1 の `Metric { Count, Sum, Avg, Min, Max }` と `Aggregate { group_by, metric, field }` を拡張。

- 数値集計は `data_index_num` の `val numeric` で計算（**正確な小数**）
- グループ化は `data_index_text` の値ブロックで区切る
- **可視行に限る**（行述語を索引内で交差）
- **スモールセル抑制（`DEFAULT_AGGREGATE_MIN_ROWS = 5`）を v1 のまま継承**（PIT-17）
- 集計クエリ自体の監査記録も継承（反復差分攻撃の検知可能性）

### 5.6 エラーと HTTP ステータス（Phase 15 実装で整理）

設計時の本書と roadmap は入力の拒否を一律「422」と書いていたが、実装は **2 系統**に分けた。
写像の正本は `crates/api/src/error.rs` の `From<data::DataError> for ApiError`。

| `DataError` | HTTP | 本文 | 使う場面 |
|---|---|---|---|
| `InvalidInput(InputError)` | **400** | `{status, title, code, message, field?}`（検証 C-P2-4 で構造化） | 入力値・スキーマ・クエリ指定の誤り全般。`code` は `cursor_invalid`（改竄・鍵の入替え・形式不正）/ `offset_not_supported` / `field_not_indexed` / `field_not_sortable` / `value_type_mismatch`（日付の列に日付でない比較値を含む・ユーザー・部署の列の比較値が ID の形式でない。**形式だけを見て存在は問わない**＝ディレクトリから消えた退職者の ID でも絞り込める。**設計上の限界**: ユーザー ID の形式は「空白を含まない ASCII の 1 語」なので、ASCII 1 語の表示名（`Bob`）やメールアドレスは形式の検査を通り、その ID のユーザーがいなければ 400 ではなく 0 件になる。日本語や空白を含む表示名は 400 で止まる。表示名から ID を引くのはクライアント（ピッカー）の役目）/ `value_out_of_range`（`numeric_spec`・日付は 0001〜9999 年・閏秒不可）/ `value_too_long`（**文字数**＝Unicode スカラー値で数える。テキスト 10,000 字・テーブル名 128 字）/ `required_missing` / `invalid_option` / `too_many_values` / `schema_invalid`（定義を書いた本人へ理由を返す）/ `snapshot_unsupported` / `fsm_invalid`（作成時に初期状態以外を指定・状態を直接更新・定義外の遷移）/ `schema_conflicts_with_data`（値の無い行がある列の必須化・行のあるテーブルへの必須の新しい列の追加・使われている選択肢の削除。列の表示名と件数つき）/ `invalid_input`（NUL 文字・`limit` が 0 で件数だけでない・最上位の未知のキー・形の誤り）。`message` には**値・閲覧者が読めない列名・内部 ID・`record_int`・SQL 断片を入れない**（文言を作る入口は `InputError` の構築子だけ。書込の検証は書き手が名指ししていない列の名前を外す） |
| `Invalid` | **400** | `{status, title, code: "invalid_input", message}`（汎用の文言・理由はサーバのログだけ） | 理由をまだ構造化していない入力の誤り（理由の文言を利用者へ返さない） |
| `Rejected(QueryRejection)` | **422** | `{status, title, code, message, hint?, location?, limit?, expected?}`（OpenAPI は `QueryRejectionBody`） | **入口の拒否**: SQL 前段面の部分集合外・名前解決不能（`syntax_error` / `unsupported` / `unknown_name` / `invalid_join` / `invalid_query`）・入口の資源上限（`limit_exceeded`・`limit.name` に `max_sql_bytes` 等・IR 直投入にも同じ上限）・`max_join_depth`・**ViewSpec の保存時検証**（`invalid_query`、ルックアップ列の上限は `limit_exceeded` / `max_view_lookup_columns`） |
| `QueryTooExpensive` | 400 | `{code: "query_too_expensive", suggestions}`（`narrow_filter` / `promote_index` / `composite_index`） | 受付制御（[query-optimizer.md §5.4](./data/query-optimizer.md)）。推定値は含めない |
| `Forbidden` | 403 | `{status, title}` | テーブルは見える（viewer）が操作の relation（editor / owner）が無い・行の書込条件・actor 述語を満たさない |
| `FieldUnavailable` | 403 | `{status, title, code: "field_unavailable", message}`（`ForbiddenBody`） | 存在しない列・読めない（マスクされた）列の指定（**両者で同一の本文**・列名なし・§5.2・§16.1） |
| `NotFound` | 404 | — | 不可視行・存在しない行（区別しない）。**テーブルが見えない（viewer でない）・存在しない・削除済み**も区別しない（§16.1・検証 2-A P3） |
| `RevConflict` | 409 | `code: rev_conflict`・`current_rev` | 楽観ロック |
| `UniqueViolation` | 409 | `code: unique_violation`・`field?`（参照名）・`message`（表示名つき） | unique 宣言の違反・一対一 / 一対多のリンクの参照先の一意の違反（値・相手の行は返さない。列は書き手が読める列のときだけ名指しする） |
| `DerivedRecomputing` | 409 | `code: derived_recomputing`・`field` | 材料化した派生列が再計算中（§7.4） |
| `IndexPreparing` | 409 | `code: index_preparing`・`field`・`progress`（0〜1 か null） | 索引のバックフィル中（§10） |
| `IndexApprovalRequired` | 409 | `code: index_approval_required`・`field`・`indexed_fields`・`max_indexed_fields` | 索引列数の上限超過（§10） |
| `NameTaken` | 409 | `code: name_taken` | 同じ名前のテーブルが既にある |
| `Conflict` | 409 | `code: conflict` | 取込 ID の再利用・並行のスキーマ切替など |
| `Gone` | 410 | — | スナップショット読取の基準位置が保持期間外（§3.3） |
| `ReadOverloaded` | 429 | 固定文言 | テナントの読取の同時実行の枠が空かない（§16.8・再試行可） |
| `ReadTimeout` | **503** | `{status, title, code: "read_timeout", message}`＋`Retry-After` | statement timeout（SQLSTATE 57014）で打ち切った（再試行可・SQL や推定値は返さない） |

> 補足（検証 C P3）: 直接の要求の投影（`select`）に読めない（マスクされた）列を入れると **403**（条件・並べ替え・
> SQL 面と同じく黙って落とさない）。保存ビューの投影は実行前に閲覧者が読める列へ絞る。HTTP の
> `POST /data/tables/{id}/query` とゲートウェイ・チャット・ワークフローのクエリは、最上位の未知のキー
> （`limitt` 等）を `invalid_input`（`field` にそのキー）で拒否する（保存済みのビューの読み戻しは寛容なまま）。
> 件数の形は 2 つある（意図的に揃えていない・互換のため）: `POST …/query`（`with_count: true`）の
> `QueryResult.count` は `CountResult`（`{"exact": n}` か `{"at_least": n}`）、v1 互換の
> `GET /data/tables/{id}/records/count` は `{"count": n, "at_least": bool}`（`at_least: true` なら n 件以上）。
> どちらも同じ上限付き件数（§5.4）。`limit: 0` ＋ `with_count: true` は件数だけを取る（ページは空）。
> カーソルは**テーブル・クエリの形には束縛するが利用者には束縛しない**（他人のカーソルを使っても、結果は
> 使った人の権限で絞られるので漏洩はない）。

**要求の形の誤り**（本文の JSON・パス・クエリ文字列の読取失敗・ルートやメソッドが無い）は、axum の抽出器の
英語の平文ではなく、サーバ全体で `RequestErrorBody`（`{status, title, code, message}`・400 は `code: "invalid_input"`、
ほかは `unsupported_media_type` / `payload_too_large` / `not_found` / `method_not_allowed`）に寄せる
（`crates/api/src/server/rejection.rs`・検証 N14）。文言は日本語で、serde の英語・内部の型名を含めない
（項目名と位置だけを拾う）。SSE の不正な `Last-Event-ID` は 400 `cursor_invalid`。`limit` の上限超過は
上限で切り詰める（応答の件数が上限になる・下限の 0 未満は 400）。

400 と 422 を分けた理由: `Invalid` → 400 は Phase 9（v1）以来の API 全体の慣例で、15.1 以降もそれを変えなかった。
422 は**構造化した理由を返して LLM・画面に自己修正させる**入口（§16.1 のエラー契約・§8.2）に限った。
どちらも `code` を持つが、422 は自己修正の手掛かり（`hint` / `location` / `limit` / `expected`）まで持ち、
400 は `code` と安全な `message`（と `field`）だけを持つ。クライアントは状態コードで出し分けられる。

---

## 6. 認可（4 階層の維持）

**v1 の 4 階層をそのまま維持する。変えるのは述語の評価場所だけ。**

| 階層 | v1 | v2 |
|---|---|---|
| ① テーブル ReBAC | OpenFGA `data_table` viewer/editor/owner | **変更なし** |
| ② 行述語 ABAC | `PolicyExpr` → `WHERE` 断片へコンパイル・値は必ずバインド | **同じ AST・同じ材料解決。出力先が索引テーブル条件になる** |
| ③ フィールドマスク | `field_policy` で応答から除去＋filter/sort から 403 | **変更なし**（§5.2 に継承）。**Phase 15 で追補**: 読めない列には作成・一括取込でも書けない（更新と同じ 403）。読めない列に依存する派生列（数式・計算・ルックアップ・ロールアップ。経由するリンク列か参照先で辿る列が読めない）も、その閲覧者には読めない列と同じ扱い（応答・検索・SSE・閲覧者文脈で一律）。履歴は読めない列の変更記録を返さない（読めない列だけの版は版ごと返さない）。**テーブル定義の応答**（`GET /data/tables`・`GET /data/tables/{id}`・ゲートウェイの `data.schema`）は、スキーマ管理者（テーブルの owner）以外には**読める列だけ**を返し、`field_policy`・`row_policy`・RAG の投影設定を返さない（検証 C-P2-5・`DataStore::get_table_for_viewer`） |
| ④ 行個別共有 | `data_record` スパースタプル（`= ANY($n::uuid[])`） | **タプルは変更なし**（uuid のまま）。材料解決時に `record_int` へ引き直して `= ANY($n::bigint[])` で効かせる |

> **テーブルの共有（#864）**: ①のタプルは、テーブルの **owner だけ**が `PUT / DELETE / GET /data/tables/{id}/shares`
> （`DataStore::share_table` / `unshare_table` / `list_table_shares`）でユーザー・部署（`role#member`）へ付け外しできる。
> 付与できる役割は **viewer / editor だけ**で、owner は付け外ししない。見えないテーブルは 404、見えるが owner でなければ 403
> （**ミニアプリ所有のテーブル（`app_id` あり）は付与・解除・一覧とも 403**: アクセスはアプリのインストールとスコープで配るので、
> インストールした人が owner を持っていても /data からは触らせない（インストールが付けたロールのタプルを外させない）。拒否は監査に deny）。
> 削除済みのテーブルは、タプルが残っている相手にも 404。共有先は id の形と同一テナントのディレクトリに
> 居ることを検査し（レコードの個別共有と同じ・存在しない相手は 400）、監査（`data.table.share` / `unshare` / `shares.list`）に
> 失敗したらタプルを補償する。共有しても行述語・フィールドマスクは開いた人の権限で効く。付け外しの後は
> `after_table_rebac_changed` を呼ぶので、**取消は即時に反映**する（REST は次の要求から 404、開いている購読は TTL を待たず
> 次のバッチで `stream.reset{authz_changed}`）。材料化の前提（§7.4）もその場で検査し直す。

### 6.1 材料解決

`crates/data/src/policy/material.rs` をそのまま使う。**キャッシュ禁止の原則も維持**
（権限剥奪・ロール変更・共有解除の即時反映のため）。`MAX_ROLE_SET = 1000` 超過は fail-closed、
`MAX_SHARED_IDS = 10_000` 超過は切り詰め＋`shares_truncated` 通知（可視が減る方向）も継承。

**例外はリアルタイム配信のみ**（§9.4 で TTL 付き再解決を明示の約束として定義する）。

### 6.2 行述語を索引テーブルで評価する

行ポリシー「自分の申請、または自分の部門の申請」は、実行者が佐藤（営業部）のとき次に畳み込まれる
（`HasRole` はホスト側で解決済みの定数になる・v1 の挙動）。

```text
(f16 = "佐藤") OR (f20 IN ("営業"))        ← f16=申請者, f20=部門（ユーザー定義列は 16 以降・§3.2）
```

`f16` も `f20` も索引テーブルに載っているので、**この判定が索引内で完結する**。

```sql
AND ( EXISTS (SELECT 1 FROM data_index_text p WHERE p.tenant_int=$1 AND p.table_int=$2
                AND p.field_id = 16 AND p.val = $user_id AND p.record_int = i.record_int)
   OR EXISTS (SELECT 1 FROM data_index_text p WHERE p.tenant_int=$1 AND p.table_int=$2
                AND p.field_id = 20 AND p.val = ANY($dept_array) AND p.record_int = i.record_int)
   OR i.record_int = ANY($shared_ints::bigint[]) )                -- ④ 個別共有（uuid → record_int は材料解決時に引き直す・§3.2）
```

- `IsOwner` は `field_id = 0`（owner 擬似列）で同じ形になる
- `Public` は定数 TRUE、`HasRole` は定数 TRUE/FALSE に畳み込み（v1 と同じ）

### 6.3 新規制約: `row_policy` 参照フィールドは `indexed` 必須

**スキーマ保存時に検証し、違反は 400 で拒否する**（当初「422」・§5.6）。

> **v1 から移行したテーブル**（Phase 15 実装で追加）: v1 は参照列の `indexed` を求めていなかったので、そのままでは
> 以後のどのスキーマ改訂もこの検証で拒否される。索引の台帳を初期化するとき（Task 15.2 より前のテーブルの初回アクセス・
> 常駐ワーカーのスイープ）に、`row_policy` が参照する列を**自動で `indexed` に昇格**してバックフィルする
> （`field_identity_it::legacy_row_policy_on_unindexed_field_is_promoted_on_upgrade`・`ledger_init_promotes_row_policy_fields`）。

理由: 行述語を本体側で評価すると、**権限が狭いユーザーほど遅くなる**。可視率 1% のユーザーが
20 件のページを埋めるのに 2,000 件の本体を読むことになる（v1 にも存在する病）。
索引必須にすればこの構造が発生しない。

> 制約は 1 つ増えるが、**性能と厳密性の双方に効く**。ポリシーで使う列は業務上ほぼ必ず
> 絞り込みにも使うため、実運用の負担は小さい。

### 6.4 SQL 生成の安全性（PIT-21）

v1 の原則を維持する。

- **値は必ずバインド**。SQL テキストへ埋め込むのは検証済みの識別子と固定エイリアスのみ
- **v2 では埋め込む識別子すら消える**: 列は `field_id`（`int2`）としてバインドされるため、
  フィールド名を SQL に文字列展開する経路が構造的に無くなる（v1 の `data ->> 'field'` は消滅）
- 読取 SQL の組み立ては引き続き**単一の関数群に閉じる**（`query/executor.rs` の役割を継承）

### 6.5 PIT-17〜21 の継承マップ

| PIT | v1 の対処 | v2 |
|---|---|---|
| **PIT-17** 集計からの個人特定 | スモールセル抑制＋集計クエリ監査 | **継承**。抑制判定は集計結果に対して行うため方式非依存 |
| **PIT-18** 述語材料の量（FGA） | `MAX_ROLE_SET` fail-closed / `MAX_SHARED_IDS` 切り詰め | **継承**。加えて §9.4 の SSE で TTL 再解決を明示 |
| **PIT-19** マスク列でのソート/絞り込み漏れ | `ensure_queryable` で 403 | **継承・強化**（§5.2 に `search` も追加） |
| **PIT-20** テーブル間の権限素通り | lookup 解決時に参照先の行述語を適用 | **全面解決**（§7.4）。リンク・ロールアップ・数式のすべてに参照先述語を透過適用 |
| **PIT-21** WHERE 強制注入だけでは不十分 | 脅威モデルテスト 781 行（`policy_threat_it.rs`） | **継承**。v2 では**同テストを両実装（v1/v2）で走らせて parity を確認**してから切替（§13 M3） |

### 6.6 判断記録: 行の閲覧可否を事前展開しない

行述語は**読取時に索引テーブル上で評価する**（§6.2）。「レコード × 閲覧者」の可否を書込時に計算して保存しておく
（事前展開・materialized ACL）方式は採らない。

kintone はレコードのアクセス権をこの方式で実装しており、読取は速いが、事前評価の量が
「レコード数 × アクセス権の設定数 × アクセス権をもつユーザー/組織/グループの数」で膨らむ。
公式資料は、アクセス権の変更に 1 時間以上かかった例と、その間は同一ドメインの全アプリで設定変更ができず、
対象アプリへのレコード追加・更新もロックエラーになることを挙げている
（[cybozu.dev: パフォーマンスに影響を与えないアクセス権の管理](https://cybozu.dev/ja/kintone/tips/best-practices/performance/managing-permissions-not-to-impact-on-performance/)・
[アクセス権の事前評価とアプリの更新](https://cybozu.dev/ja/kintone/tips/development/customize/columns/access-permissions-evaluation-app-update/)）。

本設計で事前展開を採らない理由:

- **フルプール**では、1 テナントの権限変更が書込の嵐になり、共有プールの全テナントの書込レイテンシに波及する（§16.8）
- **剥奪の即時性**（PIT-18）と両立しにくい。展開の再計算中は古い可否が残る
- 読取の遅さは、`row_policy` の参照列を `indexed` 必須にし（§6.3）、見える行が少ない利用者は
  行述語の和集合から入る（認可駆動・query-optimizer.md §5.1）ことで抑えられる
- 事前展開が必要になるのは、行述語が索引で評価できない形（任意の計算式など）を許した場合である。
  `PolicyExpr` は閉じた語彙なので、その形はそもそも作れない

---

## 7. リンク・ルックアップ・ロールアップ・数式

### 7.1 リンク（双方向）

`data_link` の 1 テーブルで両方向を賄う（§3.3 の逆引き索引）。

```text
順方向行: (table_int=案件, field_id=f9,  src_record=proj_A,   dst_table_int=顧客, dst_record=cust_001)
逆方向行: (table_int=顧客, field_id=f9', src_record=cust_001, dst_table_int=案件, dst_record=proj_A)
          ↑ f9' は顧客テーブル側の対称列（symmetric_field）

「proj_A の顧客は？」  → PK で (table_int=案件, field_id=f9,  src_record=proj_A)   → cust_001
「cust_001 の案件は？」→ PK で (table_int=顧客, field_id=f9', src_record=cust_001) → proj_A, proj_B

どちらも「自分側の table_int」で引くので、両方向とも 1 パーティションに枝刈りされる。
```

**逆参照のために別の索引を用意する必要がない。** これが双方向リンク（1:1 / 1:N / N:N）を
実装する上での本方式の最大の利点。

**カーディナリティはスキーマで宣言する**（v1 の `record_ref` は単一 UUID 文字列しか受け付けず、
N:N を API から作れなかった）。

```rust
pub struct LinkDef {
    // 参照先テーブルは FieldDef.ref_table（v1 の record_ref と同じ場所）に置く
    pub cardinality: LinkCardinality,
    /// 参照先に置いた対称列（逆方向行の field_id）。サーバが採番する。None なら片方向リンク。
    pub symmetric_field: Option<FieldId>,
    /// 対称列を参照先に作るときの参照名（作成時の入力。参照先テーブルの owner 権限が要る）。
    pub symmetric_key: Option<String>,
    /// この列が対称列（逆方向・読取専用）か。サーバだけが立てる。
    pub reverse: bool,
}

pub enum LinkCardinality {
    OneToOne,    // src 側・dst 側とも 1 件まで
    OneToMany,   // src 1 件 : dst 複数
    ManyToOne,   // src 複数 : dst 1 件（v1 の record_ref と同じ向き）
    ManyToMany,  // 双方複数
}
```

> **Phase 15 実装で変更**（`crates/data/src/derived/types.rs`）: `ref_table` は `LinkDef` ではなく `FieldDef.ref_table` に置いた
> （v1 の `record_ref` の定義をそのまま使い、`link` を持たない `record_ref` を v1 互換の `many_to_one`・片方向として扱うため）。
> v1 の向きを表す `ManyToOne` を足した。対称列は入力の `symmetric_key` で作らせ、`symmetric_field` はサーバが採番する。
> 対称列（`reverse = true`）は**スキーマ上のメタデータで、値を本体に持たない**。順方向の書込と同じ Tx で書いた
> 逆方向行（`data_link.is_reverse`）を、閲覧者本人の権限で読取時に引く。対称列は読取専用で `indexed` / `required` にできず、
> フィルタ・ソートにも使えない（リンク元の列で絞る）。

- 値の表現は**常に配列**（`OneToOne` / `ManyToOne` の src 側は要素数 1 に制限）。空配列は「値なし」。
  v1 の単一 UUID 文字列からの移行は M1 で行う。
  **実装（Task 15.4 追補・`migrations/0091`）**: 書込は v1 の単一 UUID 文字列も受け付けて配列で保存し、応答は
  移行の前後どちらの保存形でも**常に配列**を返す（外部契約の変化。v1 のクライアントは文字列を受けていた）。既存レコードの
  書き換えはオンラインのチャンク処理（`crates/data/src/link_values.rs`・索引ジョブキューの `link_values`）で、テーブル単位に
  `record_int` 順に進み、冪等・中断再開でき、末尾まで進んだら全行を検査して文字列が残っていればもう一周する。
  進捗は `data_link_value_migration`、テーブルの状態は `data_table.link_value_format`（1 = 文字列が残り得る / 2 = 移行済み）
- **一意性は宣言だけでなく DB 制約で強制する**（`data_unique` はスカラー値用でリンク先をキーにしないため
  流用できない）。§3.3 の部分一意インデックス `(tenant_int, table_int, field_id, src_record) WHERE single_valued`
  1 本で両側を賄う。**2 行方式なので「dst 側の一意」は逆方向行に対する同じ制約になる**。

  | カーディナリティ | 順方向行の `single_valued` | 逆方向行の `single_valued` | 意味 |
  |---|---|---|---|
  | `OneToOne` | `true` | `true` | 双方 1 件まで |
  | `OneToMany`（src=one 側） | `false` | `true` | src は複数持てるが、各 dst は 1 つの src からのみ指される |
  | `ManyToOne` | `true` | `false` | src は 1 つだけ指す（v1 の `record_ref`）。dst は複数から指されてよい |
  | `ManyToMany` | `false` | `false` | 制約なし（`ord` の要素数上限のみ） |

  片方向リンク（`symmetric_field = None`）で dst 側一意が要る場合は、逆方向行を
  「制約専用行」として書く（`is_reverse = true` かつクエリからは参照しない）。

  > **Phase 15 実装で変更: 「制約専用の逆方向行」案は採らず、dst 側の一意が要るリンク（`OneToOne` / `OneToMany`）には
  > 対称列を必須にした**（無ければスキーマ検証で 400）。逆方向行は参照先テーブル側の `field_id` と、その列の
  > `data_link_field` の宣言（`single_valued` を固定する FK の参照先）を要する。対称列を持たない制約専用行は、
  > この `field_id` の採番・スキーマ改訂でのカーディナリティ変更・撤去の扱いが参照先スキーマの外に宙に浮く。
  > 対称列（読取専用・値は持たない）として宣言させれば、通常の逆方向行と同じ機構に載る。
  > `OneToOne` の両側の一意も `data_link` の部分一意索引だけで強制する（`data_unique` は使わない）。リンク列への
  > `unique` 宣言は受け付けない（一意性は `cardinality` で宣言する）。
- **`single_valued` はレコード書込側が決めてはならない。** 部分一意インデックスの述語にフラグを使う以上、
  書込時に `false` を渡せば制約を迂回できてしまう。`data_link_field`（スキーマ改訂時にのみ書き換わる
  宣言メタデータ）への**複合外部キーで宣言値に固定する**（§3.3）。宣言が `true` のフィールドに
  `single_valued = false` の行を挿そうとすると参照先の組が存在せず FK 違反になる。
  カーディナリティを変更できるのは**スキーマ改訂経路のみ**で、そのとき既存行の再検証を行う。
- **対称フィールド**があれば、逆方向行を**同一トランザクションで同時に書く**（§3.3 の 2 行方式）。
  順方向だけを更新して逆方向が古いままになる状態は、同一 Tx なので発生しない。
- 参照整合性は書込時にサーバが検証（v1 の `validate.rs` を継承）
- `ord` の要素数には上限を設ける（PIT-72）

### 7.2 ルックアップ

参照先の列の値を引いて表示する。

```text
① 案件を 20 件取得（f9 に顧客 ID が入っている）
② その顧客 ID 20 件で、顧客テーブルの索引/本体を 1 クエリで引く
   ← このとき参照先テーブルの行述語を「閲覧者本人の権限で」適用する
③ 見えない顧客の値は null（存在秘匿）
```

v1 の `select_lookup_values` が既にこの形。**バッチ取得（フィールドごとに 1 クエリ）を維持**する。

> **Phase 15 実装で追補（存在秘匿をリンク列の値にも掛ける）**: 順方向のリンク列の値（参照先 id の配列）も、
> 読取の出口で**閲覧者本人の権限で見える参照先だけ**に絞る（参照先テーブルの viewer・行述語の二段。リンク列ごとに
> 1 回のバッチ取得でルックアップと共用）。REST・SQL・チャット・SSE・ビュー・ミニアプリ・ワークフロー出力・
> 更新の応答・履歴の差分の全面が同じ出口（`crates/data/src/derived/mod.rs`）を通る。保存値は書き手が付けた全参照先のまま。
> 書込は**今回足した参照先だけ**を書き手の権限で検証し、書き手に見えない既存の参照先は元の位置に残す
> （見えている並びをそのまま送り返せば差分なし・`crates/data/src/link_merge.rs`）。単一値のリンクで見えない参照先を
> 別の参照先へ付け替えるのは 403（黙って外させない）。

### 7.3 ロールアップと数式

- **ロールアップ**: リンク先の集合に対する集計（`count` / `sum` / `avg` / `min` / `max` / `concat`）
- **数式**: 自由文字列を SQL にしない方針を維持し、**閉じた AST ＋ Rust インタプリタ**で評価する

```rust
pub enum FormulaExpr {
    Field(FieldId), Lit(Value),
    Add(..), Sub(..), Mul(..), Div(..),            // 算術（numeric）
    Concat(Vec<FormulaExpr>), Upper(..), Lower(..), // 文字列
    DateAdd(..), DateDiff(..), Now,                 // 日付
    If(Box<FormulaExpr>, .., ..), Switch(..),       // 条件
    And(..), Or(..), Not(..), Cmp(..),              // 論理
    Rollup { link: FieldId, target: FieldId, op: RollupOp },
}
```

> **Phase 15 実装で変更**（`crates/data/src/formula/ast.rs`）: `Field` と `Rollup` の `link` / `target` は `FieldId` ではなく
> **参照名（key）**で書く（script・IR・保存ビューと同じ外部契約。key は不変なので参照が失効しない）。小数 `n` 桁（0〜9）への
> 四捨五入 `Round` を足した（`Div` は被演算子の小数桁の大きい方 + 6 桁へ偶数丸め）。`DateDiff` は **UTC の日付での日数の差**
> （`to − from`）、`DateAdd` は日数（整数）を足す。式の深さの上限 `MAX_FORMULA_DEPTH` は 16。

- **依存 DAG をスキーマ保存時に構築し、循環を検出して 400**（当初「422」・§5.6。`MAX_FORMULA_DEPTH` も設ける）
- 算術は `numeric`（金額の正確性）
- LLM / 開発者由来の任意式が SQL になる経路を作らない（v1 の設計原則を継承）
- **`Now` を含む式は `Volatile`**（§7.4）。閲覧者には依存しないが**時刻に依存する**ため、
  `row_policy` の有無だけで `Invariant` と判定して材料化すると、`DateDiff(Now, due_date)` のような列が
  次のレコード更新まで永久に古い値を返す。読取時計算のみに限定する
  （時刻粒度を決めた定期再計算を選べるようにするのは将来項・§15）

### 7.4 マテリアライズ可否 — **authz 不変性で決める**

**これが本章で最も重要な規則。後から入れることが構造的に不可能なので、最初から型に入れる。**

ロールアップの合計値を書込時に計算して保存すると、**閲覧者ごとに違うはずの値が同じになる**。

```text
経費テーブルに 10 件（合計 100 万円）
  部長には 10 件全部見える     → 合計 100 万円
  一般社員には 3 件だけ見える   → 合計 30 万円であるべき
  ↑ 保存してしまうと一般社員にも 100 万円が見え、見えない 7 件の情報が漏れる
```

```rust
pub enum Materialization {
    /// 全閲覧者・全時刻で同値と証明できる → 書込時に材料化・索引可・ソート/フィルタ可
    Invariant,
    /// 閲覧者ごとに値が変わる → 読取時計算のみ・キャッシュ禁止・索引不可・ソート/フィルタ/集計不可
    PerViewer,
    /// 時刻に依存する（`Now` を含む）→ 読取時計算のみ・索引不可
    Volatile,
}
```

#### `Invariant` の判定には 4 階層すべてを見る（重要）

当初「参照先に `row_policy` も `field_policy` も無ければ `Invariant`」としていたが、**これは不十分**。
行集合を変える要素は 4 階層すべてにあるため、**1 つでも閲覧者依存があれば `PerViewer`** に倒す。

| 階層 | `Invariant` の条件 | 破れたときの漏洩 |
|---|---|---|
| ① テーブル ReBAC | **参照先テーブルの viewer 集合が参照元の viewer 集合を包含すると証明できる** | 参照先テーブルを見られないユーザーに、参照先の全件から計算した合計が参照元経由で見える（**第 1 層の迂回**） |
| ② 行述語 `row_policy` | 参照先に未定義 | 見えない行の値が合計に混ざる |
| ③ フィールドマスク `field_policy` | 参照先の対象列にマスク未設定 | マスク列の値が集計値から逆算できる |
| ④ 行個別共有 | **参照先テーブルに `data_record` スパースタプルが 1 件も存在しない** | 共有の有無で行集合が変わる |

①の「包含を証明できる」は実務上ほぼ成立しないため、**既定は `PerViewer`**。
`Invariant` を許すのは、参照元と参照先が**同一ミニアプリの所有テーブル**であり、
同一の ReBAC 主体集合に束ねられていることをスキーマ検証が確認できた場合に限る。

#### 降格と再計算のトリガ

材料化データは「作った時の前提」が崩れた瞬間に漏洩源として残る。次のいずれでも
**同一トランザクションで `PerViewer` へ降格し、材料化データと索引行を破棄する**。

- 参照先に `row_policy` / `field_policy` を追加した
- 参照先テーブルの ReBAC が変わり viewer 集合の包含が崩れた
- 参照先に**レコード個別共有タプルが 1 件でも作られた**

さらに、**参照先レコードの変更でも材料化値は陳腐化する**（金額の更新・行の作成/削除・リンクの付け替え）。
依存 DAG はスキーマの循環検出だけでなく**実データの逆依存追跡**にも使い、
参照先の変更トランザクションから影響する参照元を特定して、
①同一 Tx で再計算するか ②世代付きジョブへ確実に enqueue し、**再計算が完了するまで当該列を
クエリ対象外にする**（古い値を返さない）。

- **スキーマ保存時に参照先を推移的に辿って判定する**（多段参照は 1 つでも閲覧者依存があれば `PerViewer`）
- `PerViewer` / `Volatile` なフィールドへの `indexed` / `unique` 宣言は**拒否**（400・§5.6）
- ロールアップのスモールセル抑制は参照先の `aggregate_min_rows` を継承
  （**Phase 15 実装で追補**: 判定は閲覧者に見える参照先の件数だけで行い、全行を見られる人にも同じ規則で掛かる。抑制した列は
  値を null のままレコードの `suppressed_fields` に参照名を載せ、画面は空欄ではなく「件数が少ないため非表示」と出す）
- 詳細な脅威モデルは **PIT-71**

> **Phase 15 実装で確定した規則**（`crates/data/src/derived/`・`migrations/0090_data_derived.sql`）:
>
> - **材料化するのは `Invariant` かつ `indexed` の列だけ**。値は**索引テーブル（`data_index_num` / `data_index_text`）の行としてだけ**
>   持ち、本体 JSONB には書かない。材料化した値はフィルタ・ソートにだけ使い、**表示は常に閲覧者本人の権限で読取時に計算**する
>   （材料化値を応答に出す経路が無い）
> - **多段参照（参照先の派生列を対象にするもの）と対称列を辿るものは一律 `PerViewer`**（推移的な判定を省いて安全側に倒す）。
>   ①の包含は「同一ミニアプリの所有テーブル」かつ OpenFGA のタプル比較で証明できた場合だけ
> - 参照先の変更で陳腐化した値は再計算待ち（`data_derived_pending`・世代つき）に積み、待ちがある列はクエリ対象外（409
>   `derived_recomputing`）。影響する参照元は依存登録（`data_derived_dep`）から 1 本の索引で引く
> - ReBAC の変化による降格は、テーブルの ReBAC を crate の外で付け外しした側が呼ぶ `DataStore::after_table_rebac_changed`
>   （**そのテーブル自身の材料化列に加えて、そのテーブルを参照先にする参照元の列も検査し直す**）と、読取・クエリの入口での
>   4 階層の再検査の 2 段で行う（前者を呼び漏らしても後者で追いつく）。降格では索引行・再計算待ち・依存登録を同一 Tx で消す
> - 個別共有の有無（④）は `data_table.has_record_shares`（共有の入口がタプルを書く前に同じ行で立てる・単調）で判定する。
>   移行前から存在するテーブルは共有の有無を知る手段が無いので `true`（安全側）で初期化した

---

## 8. ビュー（ViewSpec）

### 8.1 v1 の問題

`DataViewBody.display` が `serde_json::Value` でサーバ非解釈（`crates/data/src/view.rs`）。
ビュー種別も列設定も無く、フロントが解釈規約を持てない。

### 8.2 設計

**Rust 型を単一ソースとし、ts-rs / utoipa で TypeScript を生成**（「codegen が正・手書き型を作らない」）。
実装（Task 15.5）は **utoipa → OpenAPI → `web/src/generated/api`**（`pnpm gen:api`）の 1 経路にした。
ViewSpec は REST の要求・応答にしか現れず、SSE のイベント型（ts-rs の対象）ではないため。

```rust
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ViewSpec {
    Grid(GridView),         // 表形式（既定）
    Kanban(KanbanView),     // select 列でカラム分け
    Calendar(CalendarView), // date/datetime 列で配置
    Gallery(GalleryView),   // カード
    // Form は v1 スコープ外（§15）
}

pub struct GridView {
    pub columns: Vec<ColumnConfig>,   // 並び順＝表示順・見出し・幅・固定・非表示
    pub row_height: RowHeight,
    pub group_by: Vec<GroupConfig>,   // 行グループ化（折りたたみ・並べ替えの先頭キーになる）
    pub conditional_format: Vec<ConditionalFormat>, // 条件付き書式（閉じた条件木＝クエリ IR の Condition）
    // 並べ替えはビューのクエリ（DataViewBody.query.sort）を正とし、表示設定には重複して持たない
}

pub struct ColumnConfig {
    pub field: String,          // 参照名 key（外部契約は key・擬似列 _owner / _created_at / _updated_at も可）
    pub label: Option<String>,  // このビューだけの見出し（テーブルの表示名は変えない）
    pub width: Option<u16>,
    pub hidden: bool,
    pub frozen: bool,
    /// 並べ替えに使う代替列（日本語の読み仮名列など・§3.4）
    pub sort_by: Option<String>,
}
```

- **サーバが検証する**: 存在しない `field_id`・マスク列の表示指定・`PerViewer` 列でのソート指定を拒否
- 検証の失敗は 422（`DataError::Rejected`・構造化した理由・`invalid_query`。ルックアップ列の上限超過は `limit_exceeded`）。マスク列の参照だけはクエリと同じ 403
- **1 ビューあたりのルックアップ・ロールアップ列は既定 12 本まで**（テナント設定 `max_view_lookup_columns` で変更可・1〜100）。各列が 1 ページごとに
  参照先の行述語つきの解決を要するため。超過はビューの保存時検証で 422（`limit_exceeded`）。SharePoint のリストビューも
  ルックアップ等の結合を 12 本までに制限している（[Microsoft サポート](https://support.microsoft.com/en-us/sharepoint/lists/data-and-lists/list-view-threshold-for-large-lists-and-libraries)）。
  解決は列ごとの N+1 ではなく、ページの `record_int` の集合で列ごとに 1 本の一括クエリにする（query-optimizer.md §6.4）
- 保存は v1 どおり `artifact(kind=data_view)` の枠に乗せ、ReBAC 共有と不変バージョンを継承
- **実行は必ず `run_query` を経由**し、**閲覧者本人の権限で毎回再評価**（作成者の権限を引き継がない・v1 の不変条件）

---

## 9. リアルタイム配信

### 9.1 v1 の欠落

`create_record` / `update_record` / `delete_record` は**イベントを出していない**
（outbox を出すのは FSM 遷移のみ・`crates/data/src/transition.rs`）。

### 9.2 変更ログと順序

**既存の outbox ＋ per-consumer fan-out リレー（`crates/storage/src/event.rs`）を土台にするが、
配信シーケンスは新たに足す必要がある。**

現行の `claim_undelivered` は `NOT EXISTS(outbox_delivery)` の anti-join で未配送行を拾う実装で、
doc コメントが明示するとおり **「id 順・コミット順に依存せず、後からコミットした小さい id の行も
次スキャンで拾える」**ことを狙っている。つまり**単調な配信位置を採番していない**。
`data_record_revision` の主キーもレコードごとの `rev` であって、ストリーム全体の位置ではない。

このまま `?since=<seq>` を実装すると、**並行トランザクションで小さい outbox id が後からコミットした場合や
再接続時に、`Last-Event-ID` とリビジョンを対応付けられず更新を欠落または重複させる**。

そこで次を追加する。

```sql
-- 配信台帳に単調な配信位置を持たせる。
-- 【重要】グローバルな bigserial にはしない。共有採番だと他購読者のイベントで番号に穴が空き、
-- 「連続確認済み」が定義できなくなる（穴を未確認とみなせば止まり、無視すれば取りこぼす）。
-- そこで consumer ごとの連番を払い出す。
create table outbox_delivery_seq (
    consumer text primary key,
    next_seq bigint not null default 1
);
alter table outbox_delivery add column delivery_seq bigint;
create unique index on outbox_delivery (consumer, delivery_seq);
```

`mark_delivered` は当該 consumer の採番行を `UPDATE ... RETURNING` で取り、
掴んだ件数分をまとめて払い出す。

- **この行ロックが並行リレー間の排他も兼ねる**（同一 consumer への配信は直列化され、
  別 consumer とは競合しない）
- 結果として `delivery_seq` は**購読者内で穴のない連番**になり、low-water mark が自明に定義できる
- 採番は `mark_delivered` と同一トランザクションなので、番号を払い出して配信に失敗した穴は残らない

- **`Last-Event-ID = delivery_seq`**（リレーが採番した単調位置）。`?since=<delivery_seq>` で再開する
- outbox の payload に **`(table_int, record_id, rev)`** を載せ、配信位置と revision を永続的に対応付ける
  → `Last-Event-ID` から「どの revision まで見たか」が一意に決まり、再開境界が確定する
- リプレイは `outbox_delivery`（配信位置の正本）から辿り、本文は `data_record` / `data_record_revision` を引く
- 保持期間を過ぎた `delivery_seq` を要求されたら **`stream.reset` を返してクライアントに再取得させる**
  （黙って欠落させない）

**採番だけでは足りない — 購読者ごとの送信順序も契約にする。**
`claim_undelivered` は順序非依存の実装なので、リレーが `delivery_seq = 10` を先に送り、
後から `9` を送る可能性がある。クライアントが 10 を受けて切断すると、`since=10` の再開で **9 が永久に欠落する**。

- **リレーは 1 購読者への送信を `delivery_seq` 昇順で行う**（採番→送信を同一バッチ内で昇順に揃え、
  バッチをまたいでも単調にする）
- 加えて安全側として、**再開カーソルは「連続して確認済みの位置（low-water mark）」だけを進める**。
  飛び番を受け取った時点ではカーソルを進めず、間が埋まってから進む。
  **購読者ごとの連番なので「連続」は `prev + 1` で判定でき、他購読者の採番に影響されない**
- クライアントに返す `Last-Event-ID` はこの low-water mark であり、
  **受信済みの最大値ではない**（最大値を返すと上記の欠落が再現する）

> **Phase 15 実装で変更（Task 15.6・`migrations/0093`・`0095`・`crates/data/src/stream/`）**:
>
> - **採番の単位は「購読者（consumer）ごと」ではなく「テーブルごとのストリーム」**。リレーは台帳 consumer
>   `data-stream` として outbox を claim し、`data.record.changed` を consumer `data-stream:<table_id>` のストリームへ
>   `outbox_delivery_seq` から払い出した `delivery_seq` で永続化する。番号はテーブルの中で穴の無い連番で、
>   テナント横断・テーブル横断の連番にはしない（他テナントの書込量が `Last-Event-ID` から読めてしまう・PIT-75 と同型）。
>   リレー間の排他は台帳 consumer の採番行のロックで取る（同じレコードの変更は `rev` 順に採番される）
> - 購読者ごとの差は配信時の可視性判定で付ける（§9.3）。**見えない行の番号はクライアントに届かない**ので、
>   クライアント側で `prev + 1` の連続性は判定できない。low-water mark は**サーバ側**が持つ: 配信はストリームを
>   昇順に読み、送った・送らなかったに関わらず連続して処理した位置まで進める。読んだ範囲に穴があればそれは
>   保持期間による GC なので、先へ進まず `stream.reset` を送る（黙って飛ばさない）。サーバが昇順で送るので、
>   クライアントの `Last-Event-ID` は**最後に受けた `id`** でよい
> - 採番した台帳行は `retain_until`（保持期間・既定 600 秒・`data.stream.retention_secs` で 60〜86,400）まで GC しない。
>   それより古い位置からの再開は `stream.reset`、スナップショット読取は 410（§3.3）
> - outbox の行は公開部 `payload`（`event_type`・`table_id`・`record_id`・`rev`・`change`。値は載せない・汎用の consumer も読む）と
>   **非公開部 `private_payload`**（ストリームと統計の消費側だけが読む）に分ける。非公開部は `table_int`・`record_int` と、
>   **変更前の値**（旧 owner・行述語が参照する列・対称リンク列・値頻度表の対象列の前後の値）。汎用の consumer（workflow /
>   miniapp のトリガ・RAG relay）はこの列を読まないので、内部キーと旧値が外へ出る経路が構造的に無い
> - **派生値の再配信**: 参照先の変更で表示値が変わる参照元レコード（と、リンクの付け替えで対称列が変わる参照先レコード）には
>   `change = derived` のイベントを積む。台帳 consumer の名前は設定で分けられ、同じ変更を複数のリレーが見るので、
>   元のイベント id を `data_stream_fanout` へ先に書けた 1 つのリレーだけが積む（一意制約で重複しない）。1 段だけ辿り、
>   影響が大きすぎるときはテーブル単位の `refresh` に畳む
> - **参照先のスキーマ・権限の変更**: 参照先テーブルの行述語・マスク（スキーマ改訂）・テーブル ReBAC・個別共有・削除が
>   変わると、参照元の行の値は変わらないがルックアップ・ロールアップ・リンク値の見え方が閲覧者ごとに変わる。この表を参照する
>   表（リンク列・対称列・派生列の参照先にしている表）のストリームにもテーブル単位の `refresh` を積み、開いている画面に
>   取り直させる（スキーマ改訂・削除は同じ Tx。`crates/data/src/stream/relay.rs` の `emit_refresh_for_referrers`）
> - **GC と台帳 consumer の登録の排他**: 登録・バックログの写し・GC は同じ Tx 内のロックで相互に排除し、GC は
>   ロックの後に登録済みの consumer を読む。GC で消えたイベントの配送記録はスキップする（未登録のまま全バックログを拾って
>   一斉発火させない）
> - **GC の条件**: 登録済みの全台帳 consumer へ配送済みで、保持期間（`retain_until`）を過ぎた行を消す。RAG の relay
>   （`processed_at` で ack する consumer・`outbox_ack_consumer` に起動時に登録）が登録されている配備でだけ `processed_at`
>   も待つ（RAG 無効の配備で outbox が無制限に増えない）。台帳 consumer の claim は、全て配送済みと確かめた後は outbox の
>   採番が動くまで表を走査しない（アイドル時に outbox を毎秒読み直さない・`storage::event::ClaimHints`）

**配信の確定点と再送（at-least-once ＋ クライアント冪等）。**
`mark_delivered` による `delivery_seq` の払い出しは**購読者ストリームへの永続化**であり、ネットワーク送信の成功を
意味しない。送信は永続化の後に行う。送信前に確定すると欠落し、送信後に確定すると重複するが、本設計は
「永続化は送信前・到達確認はクライアントの `Last-Event-ID`」とするので、欠落は起きず重複だけが起き得る。

| クラッシュ窓 | 起きること | 再開後 |
|---|---|---|
| 永続化（`mark_delivered`）の前 | outbox に未配信のまま残る | リレーが再 claim して永続化・送信（欠落なし） |
| 永続化の後・SSE 送信の前 | クライアントは受け取っていない | 再接続時に `Last-Event-ID` 以降として再送 |
| SSE 送信の後・クライアントの確認の前 | クライアントは受け取ったが low-water mark が進んでいない | 再接続時に再送される（重複） |

- 重複は**クライアントが `(record_id, rev)` で冪等に捨てる**。既知の `rev` 以下の `record.upserted` と、
  既に表示していない行への `record.removed` は無視する。`rev` はレコードごとに単調なので判定は O(1)
- `stream.reset` を受けたクライアントは手元の状態を捨てて再取得する（冪等判定の前提が崩れたため）
- 受け入れ条件として、3 つの窓それぞれに故障を注入した結合テストを置く（Phase 15.6）

### 9.3 配信

```text
GET /data/tables/{table_id}/stream?since=<delivery_seq>     (SSE)
  event: record.upserted   id=<delivery_seq>   data={record_id, rev, fields...}
  event: record.removed    id=<delivery_seq>   data={record_id}   ← フィールドを含まない
  event: schema.changed    id=<delivery_seq>
  event: stream.reset      id=<delivery_seq>   ← 保持期間外。全件再取得を促す
```

> **Phase 15 実装の形**（`crates/data/src/stream/events.rs`・型は utoipa から TS へ生成）:
>
> ```text
> event: stream.ready      id=<delivery_seq>   data={position}                  接続直後の再開位置
> event: record.upserted   id=<delivery_seq>   data={record, derived}           record はマスク・派生値を購読者本人の権限で適用
> event: record.removed    id=<delivery_seq>   data={record_id}                 値を含まない（PIT-73）
> event: schema.changed    id=<delivery_seq>   data={table_id, schema_version}  受けたら取り直す（接続は閉じる）
> event: stream.reset      id=<delivery_seq>   data={reason}                    手元を捨てて再取得（reason = retention / authz_changed / refresh / overflow / table_deleted。接続は閉じる）
> event: awareness                              data={principal, record_id, field, at}  データを含まない（§9.5）
> event: records.bulk_applied id=<delivery_seq> data={table_id}                一括取込で行が作られた（件数を含まない・取り直す・検証 D-11）
> event: keepalive                              data={at}                        15 秒ごと（途絶したらクライアントが繋ぎ直す・検証 D-2）
> ```
>
> - **一括取込の行は 1 件ずつ配らない**: 取込が作った行は、ストリームの続く範囲ごとに `records.bulk_applied` 1 件に
>   まとめる。行ごとの可視性は判定せず（10 万行で 2 分かかっていた）、**件数・行・値を含めない**（見えない行まで数えた
>   件数は「見えない行が何件取り込まれたか」を漏らす）。`id` はまとめた最後の位置で、`Last-Event-ID` で欠落なく再開できる。
>   クライアントは「取り込まれた」と案内し、利用者の操作で取り直す（見える行だけが返る）。汎用の outbox consumer が読む payload は変えない
>   （ワークフロー・ミニアプリのトリガ・RAG は従来どおり 1 件ずつ受ける）。consumer は 1 回の claim を 500 件にして
>   取込の直後の滞留を短くした（検証 D-14）
> - `keepalive` はコメント行の keep-alive に加えて送る名前つきのイベント（ブラウザの EventSource はコメント行を読めない）
> - **配信の経路は HTTP/2 を前提にする（運用）**: 購読は表示中のタブごとに 1 本の長い接続を持つ。HTTP/1.1 では
>   ブラウザが同じオリジンへの接続を 6 本までに制限するため、購読中のタブが 6 つあると 7 つ目のタブの REST・購読が
>   待たされて開かない（検証 2-D-P3-5）。ブラウザに面するロードバランサ／リバースプロキシ（web の前段）は HTTP/2
>   （または HTTP/3）で終端すること。web は非表示のタブの購読を止めて表示に戻ると `since` で続きから繋ぎ直すので、
>   HTTP/1.1 でも裏のタブは接続を消費しないが、**同時に表示している**ウィンドウが 6 つを超える使い方は HTTP/2 でしか
>   成り立たない（タブ間での購読の共有はしていない）
>
> - `derived = true` は派生値だけの再配信で `rev` は進まない。クライアントは**同じ `rev` でも**（手元以上なら）派生値を適用する。
>   それ以外の `record.upserted` は `rev` が手元より大きいときだけ適用する
> - 再開は `?since=` か `Last-Event-ID`（不正な値は 400）。購読は viewer だけ（`GET /data/tables/{id}/stream`・
>   awareness の送信は `POST /data/tables/{id}/awareness`）

**購読者ごとに行述語とフィールドマスクを再評価してから配信する。**
見えない行・見えない列は物理的にストリームに乗らない。

#### 可視性を失った行にも必ずイベントを送る

更新後の値だけで可視性を判定して「見えない行は流さない」とすると、**更新によって可視 → 不可視に
変わった購読者には何も届かず、ブラウザは以前受け取った機密値を表示し続ける**。削除でも同じことが起きる
（更新後の行が存在しないため）。これは「見せない」の実装が配信側で破れる典型例。

そこで **before / after の双方で可視性を判定**し、遷移で場合分けする。

| before | after | 送るもの |
|---|---|---|
| 不可視 | 不可視 | 何も送らない |
| 不可視 | 可視 | `record.upserted`（新規出現として） |
| 可視 | 可視 | `record.upserted`（マスク適用後） |
| **可視** | **不可視** | **`record.removed`（record_id のみ・値を含まない）** |
| 可視 | 削除 | `record.removed` |

before の可視性は、書込トランザクションが outbox へ**変更前の述語評価に必要な最小限のキー**
（旧 owner・旧述語参照列の値）を載せることで判定できる。
権限側の変更（ロール剥奪・共有解除）で不可視化した場合は §9.4 の即時無効化で購読ごと落とす。

### 9.4 ファンアウトのスケール

素朴には 1,000 購読者 × 100 更新/秒 = 10 万回/秒の判定になり破綻する。2 つの工夫で抑える。

1. **250 ms 窓でバッチ**（1 件ずつ送らない）
2. **述語をロール共通部とユーザー固有部に分解してから**グループ化する

素朴に「畳み込み後の SQL ＋ バインド値のハッシュ」でグループ化すると、**`IsOwner` を含む行ポリシーでは
バインド値に各購読者の principal id が入り、個別共有があれば `shared_ids` も購読者ごとに違う**ため、
同じロール構成でも一致しない。この一般的な条件では 1,000 購読者がほぼ 1,000 グループになり、
削減がまったく効かない。

そこで述語を 2 部に分けて評価する。

```text
行述語 = ロール共通部 ∨ ユーザー固有部
          ↑ HasRole / FieldCmp(定数) / Public   ↑ IsOwner / 個別共有 id
```

- **ロール共通部**は畳み込み後の SQL＋バインドが一致するのでグループ化できる。
  グループごとに 1 回だけ評価する（1,000 人が 3 ロールなら 3 回）
- **ユーザー固有部**は SQL を発行しない。バッチ内の変更レコードについて
  `owner`（擬似列 `field_id=0`）と共有 id はイベント側に載っているので、
  **購読者が持つ principal id / 共有 id 集合との突合わせ（O(1) のハッシュ集合判定）**で済ませる
- 両者の論理和が最終的な可視性

これにより **評価回数はロール種類数に比例**し、購読者数には比例しない。
ユーザー固有部は集合演算のみなので購読者数に線形だが、SQL を伴わないため桁が違う。

**受け入れ条件には「`IsOwner` ＋ 個別共有を含む行ポリシーで、購読者 1,000 人・
ロール 3 種のときに述語評価が 3 回であること」を含める**（グループ化が効かない実装を通さない）。

**権限材料の再解決について（明示の製品約束）**:
材料解決は毎回 OpenFGA を叩く仕様（PIT-18・キャッシュ禁止）だが、SSE で毎更新ごとに叩くと OpenFGA が持たない。
→ **購読中は TTL（既定 5 秒）で再解決する。したがって権限剥奪の配信への反映は最大 TTL 分遅れる。**
これは設計上の妥協であり、**製品の約束として文書化する**（→ PIT-70）。
ロール変更・共有解除・テーブル権限剥奪のイベントを検知した購読は TTL を待たず即時無効化する。
なお **REST の読取経路は従来どおり毎回解決**であり、遅延は SSE 配信にのみ適用される。

> **実装（Task 15.6）**: TTL は設定 `data.stream.authz_ttl_ms`（既定 5,000・100〜60,000 ミリ秒。範囲外は起動エラー）。
> 実際に効いている値は閲覧者の文脈（`stream_revocation_delay_ms`）として画面へ返し、グリッドが「権限の変更の反映は最大◯秒遅れる」を
> 出せるようにした。即時無効化は DB の世代（`data_stream_authz_epoch`・テーブル単位とテナント全体）で行う。レコードの個別共有・
> 共有解除・テーブル削除は data が自分で、ロールの付け外し（IdP 同期）とテーブルの ReBAC の付け外しは変えた側が
> `DataStore::notify_authz_changed` / `after_table_rebac_changed` で世代を進め、配信は毎バッチ（250 ms）世代を読み比べる。
> 引き直した材料が前回と違えば（viewer の喪失・ロール・共有・マスクの変化）、行ごとの差分では取りこぼすので
> **購読ごと `stream.reset{reason: authz_changed}` で閉じる**。テーブルの editor の得失（共有の viewer ⇄ editor の変更）も
> 見える行は変わらないが画面の操作（編集・追加・取込）が変わるので同じく閉じ、グリッドは閲覧者の文脈を取り直す。

### 9.5 編集の並行制御

**CRDT（Yjs）は採らない。** 行・列単位で可視性が異なるため購読者ごとにドキュメントを分割する必要があり、
authz モデルと正面衝突する（`crates/collab` はノート/スライド用途に留める）。

- 編集は**サーバ権威**＋ v1 の `rev` 楽観ロック（競合は 409）
- 他ユーザーのカーソル・選択範囲は **awareness チャネル**（データを含まない）で配信

---

## 10. 索引の自動昇格

Lists / Teable の UX は「どの列でも絞り込める」だが、本方式で全列を索引すると
40 列テーブルで 1 レコード 40 行になる（§3.7）。

**本方式は索引の付け外しが行の追記/削除なので、オンラインで昇格できる。** これを UX の武器にする。

```text
ユーザーが「備考」列でフィルタしようとする
  → 未索引 → 403 ではなく「この列での絞り込みを有効にしますか？」を提示
  → 小さいテーブル（2 万行未満）は即座にバックフィルして透過的に有効化
  → 大きいテーブルは jobq でバックフィル（進捗表示・中断再開可）→ 完了後に有効化
```

### 索引状態は 3 値にし、building 中は write-through する

「進行中はクエリに使わない」だけでは**競合を防げない**。バックフィルがあるレコードを走査した**後**、
その列が有効化される**前**に同じレコードが更新・作成されると、書込経路が building 中の索引へ
反映しない限り、**その変更は完成後の索引から永久に欠落する**（走査済み領域は二度と読まれないため）。

```text
absent  ──promote──▶  building  ──収束──▶  active  ──demote──▶  absent
                        ↑                     ↑
                 新規書込は write-through   通常運用
                 （スナップショット走査と並行）
```

1. 索引状態を **`building`** にする（この時点から**新規書込は索引へ write-through**）
2. スナップショット（開始時点の可視行）をチャンク走査してバックフィル
3. 走査完了かつ差分が収束したら、**原子的に `active` へ切り替える**
4. `building` の間は当該列をクエリの駆動にもフィルタにも**使わせない**（半端な索引で結果を欠落させない）

- バックフィルは `crates/jobq` のバッチジョブ。**チャンク単位・冪等・中断再開可**
- 逆方向（一定期間使われない索引の降格）も同じ機構で行える
- **テーブルあたりの索引列数に上限**を設け、超過時は明示承認を要求する（無自覚な肥大化を防ぐ・→ **PIT-72**）。
  **既定は 20 列**（テナント設定で変更可・承認つきで超過可）。システム擬似列は数えない
- **ビューの保存と、グリッドでの並べ替え・絞り込みを昇格の契機にする。** 2 万行未満は即時、それ以上は裏で作り、
  完了まではその列での操作を「準備中」と表示する
- 数値は SharePoint Online の挙動を叩き台にした（索引は 1 リスト 20 本まで・保存済みビューと並べ替えで自動作成・
  2 万件以上は裏で作成。[PnP: Large lists and large libraries](https://pnp.github.io/community-docs/articles/large-lists-large-libraries-in-SharePoint.html)）。
  SharePoint（オンプレ）は閾値超過後に索引を作れなかったが、本方式はオンラインのバックフィルなのでその制約は無い
- v1 の `CREATE INDEX` 方式ではこの挙動は危険で選べなかった

> **Phase 15 実装で確定した形**（Task 15.6・`crates/data/src/index/promote.rs`・`crates/api/src/routes/data_indexes.rs`）:
>
> - **自動昇格の契機はビューの保存と、/data グリッドのクエリ（`promote_indexes: true`）だけ**。グリッドの UI は利用者の
>   並べ替え・絞り込みの操作でだけこのフラグを送る。チャット・ワークフロー・ミニアプリ（gateway）の経路は `DataQuery` そのものを
>   受けるのでフラグを持たず、未索引の列は 400 のまま（意図しない経路で索引を増やさない・
>   `data_query_surfaces::only_the_grid_request_can_ask_for_index_promotion`）。明示の昇格は `POST /data/tables/{id}/indexes`、
>   状態（`absent` / `preparing` / `active` と進捗 0〜1）は `GET` で引く
> - 昇格は**閲覧者でも上限内なら起こせる**（グリッドの並べ替えの UX）。マスク列は 403、閲覧者ごとの派生列は索引にできない（400）のまま。昇格は監査に残す
> - 索引列数の上限は既定 20（擬似列とリンクを数えない・テナント設定 `max_indexed_fields` で 1〜1,000）。超える自動昇格は
>   409 `index_approval_required` で止め、**owner だけ**が `approve: true` で承認できる。上限は**自動昇格**に掛かる。
>   テーブル作成・スキーマ改訂で `indexed: true` を宣言するのは owner（スキーマ管理者）だけの明示の操作なので、
>   それ自体を承認として扱い上限で止めない（監査に残る・検証 A P3 の仕様確認）
> - 2 万行以上のテーブルの昇格は jobq のバックフィルを積み、その列を要するクエリは完了まで 409 `index_preparing`（進捗つき）
> - **降格**: 2 万行以上のテーブルの索引行はスキーマ改訂の Tx で消さない（長い Tx でテーブル行のロックを持ち続けない）。台帳を
>   `absent` にしてクエリと書込から外し、行はジョブ（`data_index_purge`・降格時点の最大 `record_int` までを消す）がチャンクで消す。
>   後片付けが終わるまで同じ列の再昇格は 409 `index_preparing`

---

## 11. 各面からの参照

**設計の芯: 各面に個別実装を散らさず、単一の `DataQuery` / `ViewSpec` を全面が共有する。**
`workflow-engine::vocab::catalog` が「UI パレット・右パネル・AI ツール description の共通ソース」に
なっている先例と同じ流儀を data にも適用する。

| 面 | 現状 | v2 |
|---|---|---|
| **app-gateway**（B1/B2 ミニアプリ） | ✅ 9 ルート実装済み | クエリ IR 刷新に追従。`transition` を script からも使えるようにする。レコード変更の SSE は `GET /gw/data/tables/{table_id}/stream`（§9.3 の配信をアプリ所有束縛で公開・`events.subscribe` には流さない） |
| **shiki script（B2 関数）** | ✅ `Shiki.data.*`（gateway 経由） | 同上（`Shiki.data.transition` を追加・前置きは `script_runtime::prelude` で共通） |
| **shiki script（ワークフロー/skill）** | ❌ `HostBridge` が `data.*` を受けない | **`nodes/script.rs` の dispatch に data アームを追加**。api 名は `ALLOWED_APIS` に既存（`data.transition` は追加） |
| **ワークフローノード** | ❌ 語彙予約のみ・`available_stage_a` false | **`data.query` / `data.record.create` / `data.record.update` / `data.record.delete` / `data.transition` / `data.bulk_upsert` を解禁**。IR `Condition` と `DataQuery.filter` が同語彙なので変換層が不要。書込は `effect_journal`（EngineDedup）経由。schedule / event run は委譲元ユーザーの権限（workflow engine.md §6.8） |
| **generative UI** | ❌ `TableProps` は静的データ埋め込み | **`ActionBinding` に `DataView { view_id, pinned_version }` を追加**し、`data_grid` コンポーネントを束縛で描画。クライアントが送れるのは `action_id + params` のみ（`ActionDispatcher` の不変条件を維持）。実装（Task 15.7）: ミニアプリの画面の `` ```data_grid ``（中身は束ねた保存ビューの別名）。ピンは `MiniAppBody.data_views` で、保存時に保存者の権限で解決する（`MiniAppStore`・ワークフローのピンと同じ流儀）。描画は `POST /mini-apps/{id}/ui-actions`（`action_id = data_grid:{別名}`・params は `DataViewPage` の `cursor` / `limit` だけ）で、定義はバンドル権限で読み、**行・列は閲覧者本人の権限で毎回評価**する（`DataViewStore::run_via_bundle`）。#604 で tool 束縛を撤去したので束縛は 3 種 |
| **chat / agent-core** | ❌ data 系ツールがゼロ | **`data_query`（読取）・`data_record_write`（`requires_confirmation`）・`data_schema`（グラウンディング）**。`csv_tool.rs` の 3 ツールが最も近い先例。description には**発話ユーザーが viewer のテーブル一覧**を動的に載せる（`SkillTool` と同じ流儀・テーブル数は有界）。実装（Task 15.7）: `crates/chat/src/data_tool.rs`。一覧は 30 件まで、見えるテーブルが無ければ提示しない。`data_schema` は読めない列を載せない。`data_record_write` は作成・更新（status は FSM の遷移でだけ動く） |
| **RAG** | ❌ 構造化データのフックがゼロ | **§11.1**。Task 15.8 で設計スパイク＋ PoC（[data/rag-integration.md](./data/rag-integration.md)）。本番の配線はポストアルファ |

> **Phase 15 実装の補足（Task 15.7）**:
>
> - ワークフローの `data.transition` は遷移名ではなく **`table`（テーブル UUID）・`record`・`to`（遷移先の status）**（＋任意の
>   `expected_rev`）で指定する（data の FSM は「現在の status → `to`」で定義されるため）。status は `data.record.update` では動かせない。
>   `Shiki.data.transition(tableId, recordId, to, expectedRev)` も同じ形
> - 生成 UI の data 束縛は**ミニアプリの画面の `` ```data_grid ``** として実装した。会話の回答（Yui の `` ```ui ``）の中の表は
>   データ束縛を持たない（静的データのまま・未実装）
> - 全面で同じ `data::DataQuery` 型を使うことは `crates/api/tests/data_query_surfaces.rs` がコンパイル時と実行時に固定する
> - app-gateway の data ルートは、入口のテーブルだけでなく**結合・半結合・lookup / rollup・リンク値で辿る参照先もアプリ所有テーブルに
>   限る**（`data::TableReach`・`crates/app-gateway/src/routes/mod.rs` の `app_table_stores`）。アプリ所有テーブルのリンク列が
>   非所有テーブルを指すと、呼出ユーザーがそのテーブルを読めても、アプリ経由では lookup / rollup は null・リンク値は落ち、結合・半結合は 403 になる
>   （ユーザーが参照先を読めないときと同じ応答）。ワークフローの委譲（[workflow/engine.md §6.8](./workflow/engine.md) ③）も同じ仕組み

### 11.1 RAG 統合（方向性）

「権限考慮 RAG プラットフォーム」として最も効く統合であり、**本方式ときれいに嵌まる**。

- テーブル単位で「検索対象にする」をオプトイン。レコード 1 件 = 1 チャンク。投影テンプレート（タイトル列・本文列）を指定
- **pre-filter**: `data_table` を authz タグに使う（既存の folder/file タグと同型）
- **post-filter**: 行の可視性は SQL 述語なので、**候補 record_id を可視集合に絞るクエリ 1 本**で済む。
  **OpenFGA のタプルを 1 本も増やさずに行レベルの permission-aware 検索が成立する**
  （`crates/rag/src/authz_filter.rs` の file 粒度 post-filter を多型化する）
- 増分索引は §9.2 の outbox に相乗り

> ⚠️ **行の post-filter だけでは足りない（フィールドマスクの穴）**。チャンクは書込時に静的生成されるため、
> 投影テンプレートにマスク対象列を含めると、**行自体が見えるユーザーは post-filter を通過し、
> マスクされているはずの列の内容まで回答・引用に流れる**。
> 例: 全社員に見える人事レコードの給与列だけを一般社員から隠していても、RAG 経由で給与が出る。
> **v1 の対処（fail-closed・単純）**: 投影テンプレートに**フィールドマスク対象列を含められない**よう
> スキーマ検証で拒否し、対象テーブルに `field_policy` が後から付いたら索引を破棄して再構築する。
> 「候補取得後に閲覧者本人のフィールドポリシーで再投影する」方式は、チャンク本文を検索インデックスに
> 置けなくなる（本文とベクタの生成を閲覧者ごとに分ける必要がある）ため、別設計として §15 に送る。

v1 スコープでは**設計スパイクまで**（実装はポストアルファ可・Phase 15.8）。

**スパイクの結果（Task 15.8）**: 詳細設計と PoC は [data/rag-integration.md](./data/rag-integration.md)。要点:

- オプトインは `TableSchema.rag`（`title_field` ＋ `body_fields`）。投影できるのは保存値を持つ列だけで、
  **マスク対象列・派生列・参照列は 400**（スキーマ検証・§5.6）
- post-filter の入口は `DataStore::rag_visible_records`（テーブルごとに 1 本・REST の読取と同じ述語コンパイラ）。
  可視な行の投影を現在の値から返すので、レコードの hydrate も同じ 1 本で済む
- 索引の**世代**（投影の定義＋`field_policy` 全体の sha256）をチャンクに刻み、世代が変わったら索引を破棄して作り直す。
  作り直しまでの古いチャンクは post-filter が世代不一致で落とす
- 増分索引は独立の台帳 consumer（`data-rag-index`）として outbox を消費し、現在の行から投影する（`RagIndexWorker`）
- rag 側は候補を `SourceRef { File | Record }` に多型化し、post-filter を混在へ一般化した（`post_filter_sources`）

---

## 12. スケールアウト戦略（4 段の階段）

フルプールを選んだ以上、「顧客が増えたらどうするか」を設計時点で答えておく。
階段は 4 段。加えて、**階段では解けない形が 1 つだけある**ので、それは別分岐として末尾に記録する。

### 第 0 段（現状）: DB 以外は既に水平

- `shiki-server` はステートレス → レプリカを増やすだけ
- ワーカーは `FOR UPDATE SKIP LOCKED` の claim 型 → 台数追加で線形
- Tantivy は index-per-tenant、Qdrant は payload フィルタ＋クラスタ、MinIO/GCS は水平

**ボトルネックは Postgres の書き込みノードただ 1 点。** 以降はそこを開ける話。

### 第 1 段: 単一 Postgres で数十〜数百社（**一次目標・本設計で充足**）

- **索引本数がテナント数に依存しない**（§3.3）ので、テーブル数の増加では劣化しない
- 読み取りはリードレプリカへ（グリッド閲覧・集計・SSE の述語評価は読取専用）
- 重い分析は列指向スナップショット（§15）へ逃がす

### 第 2 段: テナント単位のプール分割（シャーディング）

**今やること: 継ぎ目と禁止事項の明文化のみ。実分割は需要が出てから。**

- **`tenant → pool` の解決を単一チョークポイントに置く**（`DataStore` が `PgPool` を直接持つのをやめ、
  `PoolRouter` から取る）。tenant レジストリに pool 割当を持たせる。**この継ぎ目を後から入れるのは高い**ので先に入れる
- **シャード安全の禁止事項リスト**を明文化し、CI で検査する（正本は [data/shard-safety.md](./data/shard-safety.md)・Task 15.1）:
  - テナント横断の JOIN を書かない
  - グローバル連番に依存しない（`table_int` は採番サービス経由・シャード跨ぎで衝突しないこと）
  - テナント横断の集計は「各プールへ問い合わせて合算」する形にする（単一クエリで書かない）
  - 外部キーがテナント境界を跨がない
- Notion の「Postgres 単一 → テナント単位シャード」の移行が先例

#### 発火条件（「需要が出てから」を観測可能にする）

「需要が出てから」とだけ書くと、**誰も気づかないまま限界を越える**。監視項目と閾値を先に決める。
以下は暫定値であり、アルファ後の実測で校正する（§14 の監視項目に含める）。

| 観測 | 準備開始 | 実行 |
|---|---|---|
| 書込ノードの持続書込（WAL 生成レート） | 上限の 50% | 70% |
| `data_index_*` の死行率（`pgstattuple`） | 定常 20% 超 | autovacuum が追い付かない |
| 同一操作 p95 のテナント間比 | 3 倍 | 5 倍 |

**最初に来るのは容量ではなく公平性である。** 1e8 行 ≒ 190GB（§3.7）は単一 Postgres には小さく、
容量が先に尽きることはない。先に来るのは、1 テナントの重い読取が shared_buffers と IO 帯域を
全員から奪う形である。単一 PG 内で効くのは `statement_timeout`・`work_mem`・コネクション枠までで
（§16.8）、**バッファキャッシュと IO 帯域は隔離できない**。
したがってプール分割は**テナント別 SLA を守るための手段**であり、容量の天井は後から来る。

### 第 3 段: クジラ顧客の専用棟（= cell オプションの正体）

- 巨大顧客 1 社を専用プールへ移す。**これが cell の実体**であり、
  「最初から全顧客を cell にする」のとは経済性がまったく違う
- 引越は `tenant_int` 単位の行コピー＋レジストリ切替。既存の `shiki-admin retenant`（#89）が原型

### 第 2 段で解けない唯一の形: 単一テナントが 1 ノードを超えるとき

プール分割の粒度は**テナント**なので、**1 テナント単体が書込ノードを飽和させる**場合だけは
原理的に解けない。第 3 段の専用棟に移しても「専用の 1 ノード」で頭打ちになる。

テナント内でさらに割る（`table_int` でシャード）案は取れない。**双方向リンクの書込が
src / dst の 2 テーブルに跨る**（§7.1）ため、テナント内の分散トランザクションを自作することになる。

この条件に限り、**水平分散 SQL（CockroachDB / TiDB 等）が正しい分岐**になる。
ただし**第 2 段の代替ではない。** 両者は別の問題を解いており、混同すると高い方を早く払う。

| | プール分割（第 2 段） | 水平分散 SQL |
|---|---|---|
| 解く問題 | テナント**間**の容量と公平性 | 単一テナント**内**の容量 |
| 1 レコード書込 | **ローカル Tx**（本体＋索引 N 行が同一ノード） | **毎回分散 Tx** |
| 発火条件 | 上表の 3 指標 | 専用プール上の単一テナントが書込ノードを飽和 |

#### 本設計は水平分散 SQL 上では不利な形をしている（記録）

1 書込が `data_record` ＋ `data_index_*` ＋ `data_link` ＋ outbox へ扇形に広がる（§3.7 書込増幅）。
一方 Cockroach 系のキー符号化は `/テーブル ID/索引 ID/PK…` で**テーブル ID が PK より前**に来るため、
`tenant_int` を先頭に置いても**テーブル間の共置は起きない**——1 書込が必ず 4 レンジ以上を跨ぐ。
テーブル間共置の仕組み（`INTERLEAVE IN PARENT`）は削除済みで代替が無い。
**書込増幅と分散コミットが掛け算になる**ため、移行時は書込経路の再設計（索引行の遅延反映など）が要る。

ただしその再設計は**索引と本体の非同期化**を意味する。§6.2 で行述語を索引テーブル側で評価している以上、
**「検索結果が古い」では済まず権限の誤りに化ける**。ここは移行を判断する時点の主要な設計課題として残す。

なお **Spanner は候補にならない**。GCP 専用であり、
「cloud / onprem 差は `ObjectStore` 等のトレイト裏でのみ吸収する」（design.md §1）を壊す。

### 12.1 残課題（正直に記録する）

| 課題 | 状況 |
|---|---|
| 引越中の整合（無停止移行） | 未設計。読取専用期間を許すか、二重書込＋切替かの判断が要る |
| Keycloak / OpenFGA 共有プレーンのスケール | データプレーンを割っても identity は共有のまま（PIT-26）。OpenFGA の水平分割は別課題 |
| テナント単位 PITR | フルプールでは物理 PITR で 1 社だけ戻せない。論理エクスポート/インポートによる部分復旧を用意する（design §4.12・Phase 12.10） |
| ~~`AuthzClient` に BatchCheck が無い~~ | **解決（Task 15.9）**: `AuthzClient::batch_check`（OpenFGA の `/batch-check`・50 問ごと）を追加し、data のリクエストスコープ memo から使う（§16.4） |

---

## 13. 移行計画

**本体（`data_record`）の JSONB という形は変わらない**ため、移行は思ったより軽い。

| 段階 | 内容 | 切り戻し |
|---|---|---|
| **M1: 識別子移行** | `FieldDef` 3 層化・`tenant_int`/`table_int`/`field_id` レジストリ・JSONB キーを `f{id}` へ書換 | データ書換を伴うため**最初にやる**（後続と二度手間にしない）。テーブル単位で進行 |
| **M2: 索引テーブル導入** | 索引 5 テーブルを追加し、**二重書込**（旧 partial index と新索引テーブルの両方を更新）。既存データを jobq でバックフィル | 新テーブルを無視すれば旧経路がそのまま動く |
| **M3: 読取切替** | クエリコンパイラ v2 を実装し、**機能フラグで経路を切替**。切替前に**同一クエリの結果 parity 検証**と `policy_threat_it.rs`（781 行）の両実装通過を必須にする | フラグを戻すだけ |
| **M4: 旧方式撤去** | partial index を DROP し、`crates/data/src/index.rs` と `data_index_registry` を削除 | — |

**データが小さい今が移行の最安時機**であり、顧客が増えてからでは M1/M2 のバックフィルコストが跳ね上がる。

> **Phase 15 実装の結果**: M1〜M4 を Task 15.1〜15.3 で実施した（`migrations/0084`〜`0087`）。M4 で partial index と
> `data_index_registry` を撤去し、ランタイム DDL はゼロになった（`no_runtime_ddl_lint.rs`）。ただし**本体経路
> （`crates/data/src/query/v1.rs`・本体の JSONB を直接評価する単発 SQL）は撤去せず残した**。役割は ①機能フラグ
> `SHIKI_DATA_READ_PATH=body` の切戻し先 ②必要な索引が `active` でない間（台帳が未初期化の Task 15.2 より前のテーブル・
> システム擬似列の初回バックフィル中）の正しい経路（遅いが正しい・`statement_timeout` で上限を切る） ③parity 検証の基準。
> 利用者が指定した列の索引がバックフィル中のときは本体経路に落とさず 409 `index_preparing`（§10）。材料化した派生列は
> 本体に値が無いので本体経路では扱わない。v1 の `record_ref` の値の形（単一 UUID 文字列 → 配列）の移行は M1 の追補（§7.1）。

---

## 14. 運用

- **autovacuum**: 索引テーブルは更新が多い。パーティション単位で `autovacuum_vacuum_scale_factor` を下げる
- **膨張監視**: `pgstattuple` でパーティションごとの死行率を定点観測。閾値超過で `REINDEX CONCURRENTLY`
- **EXPLAIN 回帰を CI ゲートに**: 代表クエリ集合について実行計画（駆動索引・Index Scan の使用）を
  アサートする。`crates/data/tests/data_it.rs` が既に EXPLAIN で索引使用を検証している前例を拡張する。
  **pivot 方式は駆動索引の選択を誤ると 100 倍遅くなる**ため、これは受け入れ条件に含める（→ **PIT-69**）。
  最低限カバーする最悪ケース:
  ①選択的フィルタ × 非選択的ソート／その逆 ②多条件 AND ③OR 木 ④keyset 継続（深いページ）
  ⑤**任意列（null あり）でのソート**——可視行が欠落しないこと
  ⑥**第 1 キーが 1 値に偏ったテーブルでの複数ソート**——同値群の先読み上限と複合索引の発動（§5.3）
  ⑦両方向のリンク走査がそれぞれ 1 パーティションに枝刈りされること
- **統計**: 行数カウンタ・値頻度表（種類数の少ない列は正確な件数）・フィールド統計（最頻値・ヒストグラム）・
  リンク統計を持ち、outbox 消費側と統計収集ジョブで更新する。クエリコンパイラの戦略選択に使う
  （Salesforce が自前オプティマイザを持つのと同じ理由）。詳細は [query-optimizer.md §4](./data/query-optimizer.md)。
  実装（Task 15.12・`migrations/0094`）: 統計 4 表（`data_table_stats` / `data_field_stats` / `data_value_count` /
  `data_link_stats`）。行数と値頻度は台帳 consumer `data-stats` が outbox の非公開部の前後の値からバッチで反映し、
  **冪等性はその consumer の配送記録（同じ Tx の `mark_delivered`）で担保**する。フィールド統計・リンク統計・行幅は
  jobq の統計収集ジョブ（変更行数 > 50 + 0.1 × 行数・索引昇格の完了時・推定外れの継続時に起動・テナント間で公平に claim）。
  統計の読取は `query/planner/` に、書込は `src/stats/` に閉じる（`shard_safety_lint` の PIT-75 検査）
- **走査効率を第一の指標にする**: リクエストごとの「走査した索引エントリ数 ÷ 返した行数」を記録し、
  テーブル・ビュー単位で p95 を出す。応答時間はキャッシュの温まり方や同居テナントで揺れるが、走査数は計画の良し悪しを
  直接表す（kintone も `Query_time` より `Rows_examined` を見るよう述べている・
  [Cybozu Inside Out 2018](https://blog.cybozu.io/entry/2018/08/08/080000)）。query-optimizer.md §8 のフィードバックと同じ記録を使う
- **バックフィルの可観測性**: 索引昇格ジョブの進捗・残件・ETA を管理画面に出す
- **多値上限**: `multi_select` / `link` の 1 レコードあたり要素数に上限（既定値を設定可能に）
- **本番の設定（Phase 15 で追加・`crates/api/src/config.rs` の `DataConfig`）**: keyset カーソルの鍵は
  `data.cursor_key_file`（`SHIKI__DATA__CURSOR_KEY_FILE`）か、無ければ `secrets.master_key_file` から用途別に導出する。
  全レプリカで同じ鍵が要るので、どちらも無ければ起動エラーにする（`data.ephemeral_cursor_key = true` の起動時乱数は**開発専用**）。
  SSE 配信は `data.stream.authz_ttl_ms`（既定 5,000）・`data.stream.retention_secs`（既定 600）。例は `deploy/compose/.env.example`
- **プール分割の発火監視**: WAL 生成レート・`data_index_*` の死行率・**同一操作 p95 のテナント間比**を
  定点観測する（§12 第 2 段の閾値表）。**テナント間比はこの 3 つの中で唯一「容量ではなく公平性」を
  測る指標**であり、最初に閾値を超えるのはここになる見込み。ダッシュボードに常設する

---

## 15. 将来オプション・スコープ外

| 項目 | 位置づけ |
|---|---|
| **フォームビュー（外部提出）** | 未認証提出は authz 的に別物（誰の権限で書くか）。v1 スコープ外・別途設計 |
| **添付ファイル欄・レコードコメント・通知センター** | Lists 相当機能。v1 スコープ外 |
| **列指向の分析スナップショット** | 1,000 万行の集計は行 DB が苦手。**正本は Postgres、分析ビューだけテーブル単位のスナップショットを DuckDB で持つ**。`crates/tabular`（CSV 分析に隔離 DuckDB を使用）という前例があり、**既存アーキテクチャに足すだけで何も壊れない** |
| **専用ストア（cell）／SQLite per tenant** | §12 第 3 段・判断記録は **§15.1**。強い隔離が要件化した顧客向け。**そのために物理層を Postgres 固有の飛び道具に依存させない** |
| **テナント引越ツール・実シャーディング** | §12 第 2 段。継ぎ目のみ先行 |
| **水平分散 SQL（CockroachDB / TiDB）** | §12「第 2 段で解けない唯一の形」。**単一テナントが 1 ノードを超えたときだけ**の分岐であり、プール分割の代替ではない。移植性予算は §15.1 |
| **正規表現フィルタ（`matches`）** | 索引が効かず DoS 面。採用しない |
| **RAG チャンクの閲覧者別再投影** | v1 は「投影テンプレートにマスク列を含められない」で fail-closed に倒す（§11.1）。閲覧者ごとの再投影は本文とベクタの生成を閲覧者別に分ける必要があり、別設計 |
| **`Volatile` 計算列の定期再計算** | `Now` 依存列を「時刻粒度を宣言して定期材料化」できるようにする案。v1 は読取時計算のみ（§7.3） |
| **複数ソート用の複合索引の常設** | v1 はオンデマンド材料化（§5.3）。使用パターンが固まったら常設を検討 |

---

### 15.1 判断記録: なぜ Postgres で、SQLite per tenant を採らないのか

> 設計中に「D1 / Turso 型（テナントごとに SQLite を 1 個配る）の方が、どうせシャーディングするなら
> 素直ではないか」という検討を複数回行った。**結論は Postgres 継続**だが、判断理由と
> **方針を変えるべき条件**を残す。将来「なぜ Postgres なのか」を再検討するときの材料。

#### 却下理由として挙げたが、実は弱かったもの（訂正）

**「SQLite には正確な小数型がないので金額を扱えない」——これは本設計では過大評価だった。**
pivot 索引方式では **DB は比較とソートしかしない**ためである。

- 数式・ロールアップの**演算は Rust インタプリタ側**（§7.3）。DB では計算しない
- 索引に要るのは「正しく並ぶこと」だけ → **スケール済み int64**（円なら 1/10⁴ 単位）で足りる
  （int64 は 10¹⁸ まで持てるので桁も余る）
- `SUM` はスケール済み整数のまま厳密。`AVG` は `SUM`/`COUNT` から Rust 側で出せばよい

したがって `numeric` は「あれば楽」であって「無いと成立しない」ではない。この点は却下理由から外す。

#### 正しかった指摘: 設計は論理的には持ち越せる

pivot 方式にすると **SQL を発行するのはクエリコンパイラだけ**になる（§16.1 の不変条件）。
アプリ各所に SQL が散らばらないので、**方言の差し替え先が 1 箇所に閉じる**。
「Postgres 固有機能がアプリに染み出していて移植不能」という通常の状況とは異なる。

持ち越せるもの: テーブル構成・キー設計・pivot という考え方・部分一意インデックス・
FK による `single_valued` の固定・`COLLATE "C"`（SQLite の BINARY と等価）・JSONB。

ただし**「マイグレーション不要」ではない**。差分は残る。

#### 移植性予算

以下は SQLite 比較として作った表だが、**「Postgres のどの機能に賭けているか」の一覧**として
一般化しておく。§12 の水平分散 SQL 分岐を見積もるときも、この表がそのまま材料になる。
**この表の意味は「いつか移るための準備」ではなく、「移らない判断を安全に続けるための担保」である。**
賭けている機能が増えていないことを、設計変更のたびに確認する。

| Postgres 依存 | SQLite | 水平分散 SQL（Cockroach 系） | 影響 |
|---|---|---|---|
| `numeric` | スケール済み int64 | そのまま（`DECIMAL` あり） | SQLite のみ書込パス・集計の変換（上記のとおり解ける） |
| `= ANY($1::uuid[])` | 配列型なし → `json_each` か一時表 | Cockroach は配列あり／**TiDB は無い** | **述語コンパイラの書き直し**（共有 id・ロール集合が全部これ） |
| `PARTITION BY HASH` | なし | なし（レンジ自動分割） | SQLite: テナント別 DB なら**そもそも不要**（利点）。分散 SQL: 自動分割に委ねる |
| `gin_trgm_ops` | FTS5 | **無い** | 部分一致の実装が別物。**逃がし先は Tantivy**（検索面に既存） |
| `FOR UPDATE` 行ロック | DB 全体の書込ロック | あり | SQLite のみ並行性モデルが変わる |
| outbox・監査を同一 Tx | 別 DB | 同一 Tx だが**分散 Tx** | SQLite: **リレー必須・監査ハッシュ連鎖は作り直し**。分散 SQL: 書込レイテンシ増 |
| 1 書込が本体＋索引＋リンクに跨る | 同一ファイル内 | **必ず複数レンジ**（共置不能） | 分散 SQL のみ。§12「不利な形」を参照 |
| 計画の固定（`OFFSET 0` で `EXISTS` の半結合化を止める・`CASE` で評価順を固定） | プランナが別物 | プランナが別物 | **Postgres の版の間でも実装依存**。EXPLAIN 回帰で版ごとに固定し、外れたらバッチ駆動（[query-optimizer.md §6.4](./data/query-optimizer.md)）へ逃がす |

SQLite 側はコンパイラのバックエンド 1 本分の移植であり、設計をやり直す規模ではない。
分散 SQL 側は**書込経路の再設計**が要る点だけが重く、そこは §12 に分離して記録した。

#### 決定的な理由 ①: 所在管理層

SQLite ファイルは**どこかのノードのディスク**にある。shiki-server はステートレス前提なので、
「テナント X のリクエストを X のファイルを持つノードへ運ぶ」層が新たに要る。
**ここが Postgres シャーディングとの決定的な差**である。

| | Postgres プール分割 | SQLite per tenant |
|---|---|---|
| 所在の解決 | **接続文字列を引くだけ**（`PoolRouter`・§12 第 2 段で設計済み） | ノード割当・リース・フェイルオーバー・引越・リバランス |
| シャード数 | 数個〜数十 | テナント数（数百〜数千） |
| 障害時 | Postgres の HA に乗る | ファイルの複製と昇格を自前で |
| 運用の既知性 | 既存の Postgres 運用 | 新規のステートフル部品（オンプレの部品点数も増える） |

Cloudflare は Durable Objects を、Turso は sqld を**先に作ってから** D1 を載せている。
この層は「ついでに作る」規模ではなく、テーブル基盤とは独立した分散システムの構築になる。

#### 決定的な理由 ②: 事業の形（これが答えを決める）

SQLite per tenant が**本当に勝つ**のは「**非常に多数の、非常に小さいテナント**」の形である
（Turso の売り文句がまさにそれ。テナントあたりの固定コストがゼロに近いことが効く）。

Postgres プールが勝つのは「**中規模のテナントが数十〜数百**」。
1 テナントあたりのデータが GB オーダーあり、共有プールで償却するのが効率的な形。

**本プロダクトの想定は後者（エンタープライズ数十〜数百社・§1.2）で確定している。** この形では:

- プール分割が要るのは総量 10TB 級 or クジラ顧客が出たとき。**数百社ではまだ遠い**
- そのとき必要なのは「プールを 2〜3 個に割る」であって「数百個のファイルを配る」ではない
- 隔離が契約要件になった顧客には**専用 Postgres（cell）**を出す方が、運用が既知

つまり SQLite per tenant は、**現在計画していない事業形態に最適化する選択**になる。

#### いま入れておく保険（追加コストはほぼゼロ）

1. **SQL 発行をクエリコンパイラ 1 箇所に閉じる**（§16.1 の不変条件として明記）
2. **論理設計を Postgres 固有の飛び道具に依存させない**——本設計は JSONB・btree・部分索引・FK・
   GIN という一般的な機能しか使っていない
3. **`number` 型に `scale`（小数桁数）と範囲を宣言させる**（§4）。表示・丸めの規則が決まるうえ、
   SQLite へ移す場合の「`numeric` → スケール済み整数」変換が**機械的になる**
4. **`PoolRouter` の継ぎ目**（§12 第 2 段・Phase 15.1）が、プール分割と専用ストアの**両方の入口**になる

#### 方針を変えるべき条件（これが起きたら再検討する）

| 条件 | 向かう先 |
|---|---|
| 事業が **PLG / セルフサーブで数千の小規模テナント**へ向かった | **SQLite per tenant を再検討**。この形なら所在管理層への投資が正当化される。**順序が重要で、後から数千テナントを Postgres プールから引き剥がす方が遥かに高くつく**ため、方針転換は早い段階で判断する |
| 特定顧客が**物理隔離を契約要件**にした | **専用 Postgres（cell）**。SQLite ではない（運用の既知性を取る） |
| **テナント間 p95 が開いた** or 総量 10TB 級 or クジラ顧客 | **プール分割**（`PoolRouter` の実分割・§12 第 2 段）。**先に来るのは公平性の指標**である点に注意 |
| **専用プール上の単一テナント**が書込ノードを飽和させた | **水平分散 SQL**（§12「第 2 段で解けない唯一の形」）。プール分割の代替ではなく別条件の分岐 |

---

## 16. 性能設計

> §1.2 の目標（グリッド初回描画 p95 < 300ms・1 ページ p95 < 100ms）を満たすための設計。
> **駆動索引の選択**（PIT-69）は §3.5 と §14 に既出なので、ここでは残りを扱う。

### 16.1 SQL 互換インターフェース —— 前段言語として置く

#### なぜ必要か

理由は 3 つあるが、本プロダクトで最も効くのは **LLM の出力精度**である。

「経費テーブルの承認待ちを金額順に」に対して、LLM は SQL なら高い確度で正しく書く。
一方 `DataQuery` の JSON IR は世界に存在しない記法なので、ツールの description に文法を書いても
生成が安定せず、検証エラー → 自己修正のラウンドが伸びる。
**チャットからテーブルを操作する体験（§11・Phase 15.7）の質はここでほぼ決まる。**
副次的に、BI ツール接続と、SQL DB からの移行者の学習コストにも効く。

#### 生 SQL 拒否と両立させる形

> **不変条件: 呼び出し側の SQL テキストを 1 文字もデータベースへ届けない。**

```text
SQL テキスト
  ↓ ① パース（sqlparser・既にワークスペース依存）→ AST
  ↓ ② 検証: 閉じた部分集合か（SELECT のみ・関数ホワイトリスト・サブクエリ禁止 …）
  ↓ ③ 束縛: テーブル名 → table_int、列名 → field_id（スキーマ照合。未知は 422）
  ↓ ④ DataQuery IR へ変換     ←★ 全経路の合流点。ここから先は既存と同一
  ↓ ⑤ 最適化（§16.2）
  ↓ ⑥ pivot 索引 SQL へコンパイル（行述語を無条件合成・値は全バインド）
  ↓ ⑦ 実行
```

④で必ず IR に落ちるので、**データベースへの第 2 の実行経路は生まれない**。IR は既に閉じた文法であり、
行述語の合成は⑥の単一チョークポイントにある。

> ⚠️ **「攻撃面は増えない」とまでは言えない。** DB への実行経路が増えないだけで、
> **パーサ・検証・IR 変換という新しい入力処理が増える**。ここが資源枯渇の対象になる（→ §16.1.1）。

**前例**: Salesforce の SOQL は Oracle に素通ししているのではなく、パースして自前オプティマイザが
`MT_Data` / `MT_Indexes` 向けに変換している。**pivot 索引ストアの上に SQL 互換面を載せた前例が、
採用方式と同じ出典から取れる**。

> **`crates/tabular/src/sql_guard.rs::validate_read_only` は「sqlparser で構文レベルの拒否をする」
> 実装例ではあるが、検証方針の前例にしてはならない。** あちらは
> ①`Statement::Query`（`SELECT` / **`WITH`** / `VALUES`）を通す ②危険関数の **denylist** で弾く、
> という構成で、**列挙されていない関数は通る**。しかも「ランナーの `enable_external_access=false`」
> という第 2 の壁が前提にある。SQL 前段面にはその壁が無く、**IR 変換自体が唯一の壁**なので、
> **denylist ではなく allowlist（閉じた部分集合）**でなければならない。方針が逆である点に注意する。

#### 16.1.1 入口の資源上限

`statement_timeout` は **DB の実行時間**の上限であり、パーサ・AST・コンパイルの CPU / メモリを
一切制限しない。巨大な SQL・深い条件木・大量の AST ノードやリテラルによる資源枯渇を止める上限を置く。

| 上限 | 既定値（設定可能） | 目的 |
|---|---|---|
| `MAX_SQL_BYTES` | 64 KB | パーサへ渡す前に長さで弾く |
| `MAX_AST_DEPTH` | 32 | 深い入れ子の条件木・括弧爆発 |
| `MAX_AST_NODES` | 4,096 | 横に広い式（巨大な `IN` リスト等） |
| `MAX_LITERALS` | 1,024 | リテラル数（`IN (...)` の要素数を含む） |
| `MAX_COMPILE_WORK` | 2,000,000 単位 | ①〜⑥のコンパイル全体の仕事量（ノードの訪問・比較・スキーマの走査を数える。負荷に左右されない決定的な予算） |
| `MAX_COMPILE_MS` | 500 ms | 同じ区間の壁時計の外側の安全網（仕事量の数え漏れに備える。止められたスレッドが通常の入力を誤って弾かない値） |

超過は **422 と構造化エラー**で返す（どの上限に当たったかを含める）。これらは IR 直投入の経路にも
同じ上限を課す（SQL 面だけの話ではない）。

#### 受け付ける部分集合（v1）

```sql
SELECT <列 | 集約>              -- * は禁止。射影を明示させる（マスク列の誤露出も同時に防ぐ）
FROM   <テーブル名>
[JOIN  <テーブル> ON <リンク列>]  -- 宣言済みリンク列経由のみ。任意の結合条件は不可
WHERE  <条件木>                  -- 演算子は IR の閉集合（§5.1）
[GROUP BY ...] [HAVING ...]
[ORDER BY ...]
[LIMIT n]                        -- OFFSET は禁止（keyset へ誘導・§5.3）
```

拒否: DML / DDL・CTE・ウィンドウ関数・相関サブクエリ・`UNION`・任意関数・`pg_*`・型キャスト経由の脱出。

#### JOIN の束縛規則（`ON <リンク列>` という表記だけでは足りない）

任意の結合を許すと参照先の行ポリシーの伝播が破綻するため、**SQL AST の各 JOIN を
宣言済み `LinkDef` へ厳密に束縛する**。文法上そう書けるだけでは不十分で、次を検証する。

- **`ON` 式は「宣言済みリンク列による等値 1 個」のみ。** 任意式・関数・`OR`・追加条件を拒否する
- **`ON` に条件を足せない**（`AND` で絞る書き方は `WHERE` へ回させる。`ON` へ書くと外部結合時に
  意味が変わり、認可述語の適用位置がずれる）
- **別名（alias）経由で未宣言の関係を作らせない。** 束縛先は
  `(src_table_int, field_id, dst_table_int)` の 3 つ組が `LinkDef` と一致するもののみ
- **カーディナリティも `LinkDef` から取る**（`OneToOne` / `OneToMany` / `ManyToMany`）。
  SQL 側の記述で上書きさせない
- **参照先の行述語を必ず適用する**（§7.2 の lookup と同一経路。ここを通さない結合は存在しない）
- **結合を残した集計のスモールセル抑制は、組の数ではなくレコードの個数で判定する**（独立レビュー R1-A/B で追補）。
  多値リンクの結合は 1 レコードを結合先の件数だけ組に増やすので、各グループで FROM の表のレコードの個数
  （`count(DISTINCT FROM の行)`）が FROM の表の `aggregate_min_rows` 以上であること、さらに**結合先の列を集計する**なら
  その結合先のレコードの個数が**結合先の表の** `aggregate_min_rows` 以上であることを要する（ロールアップが参照先の K を
  継承するのと同じ・§7.2）。結合先の列でグループ化するだけ・件数だけの集計は FROM の表の K で判定する
  （FROM の表のリンク列でグループ化するのと同じ情報量）
- **閲覧者にマスクされたリンク列は参照先を開かない**（独立レビュー R1-C）。半結合・結合・入れ子のどれで名指ししても、
  参照先が読めるかどうかに関係なく、存在しない列と同じ 403 `field_unavailable` になる
- 自己結合・多段結合の深さに上限を置く（`MAX_JOIN_DEPTH`）

リンク列経由なら `data_link` の 2 行方式でパーティション枝刈りも効く。
**JOIN の自由度を制限する理由は性能ではなく authz である。**

#### エラー契約 —— 列の存在オラクルを作らない

拒否理由は構造化して返す（`EmitUiTool` が検証エラー全件を返してモデルに自己修正させる前例と同じ流儀）。
**ただし「存在しない列」と「存在するが見えない列（フィールドマスク対象）」は同一のメッセージ・
同一のエラーコードで拒否する。** 区別すると、SQL 面が**列の存在オラクル**になり、
マスクした列名の有無を総当たりで確認できてしまう。テーブル名についても同様
（`data_table` の viewer でないテーブルは「存在しない」と同じ応答にする）。名前の一意性は大文字小文字を区別するので、
大文字小文字を無視した候補**すべての** viewer を先に確かめ、見える候補の中から「完全一致 → （引用符なしなら）一意」の順で選ぶ
（見えない同名異表記の有無で解決結果を変えない・独立レビュー R1-D）。

**SQL 面以外にも同じ契約を課す**（2 回目の検証 2-B-P2-1）: REST・取込・チャット・ワークフロー・スクリプト・
ゲートウェイでは存在しない列と読めない列を同じ 403 `field_unavailable` にする（§5.2）。加えて、

- 閲覧者文脈（`GET /data/tables/{id}/viewer-context`）の `masked_fields` はスキーマ管理者（owner）にだけ返す
  （それ以外は常に空）。テーブル定義はスキーマ管理者以外には読める列だけで、表示順（`order`）は詰めて返す
- 保存ビューの実行結果（`DataViewRun.display`・ミニアプリの ```data_grid）の表示設定から、閲覧者に読めない列を
  指す箇所（列設定・並べ替えの代替列・グループ化・条件付き書式・カードの列）を外して返す
- チャットの `data_schema` は読めない列の有無・件数を書かない
- マスク列への書込の拒否は監査に deny（`reason: field_unavailable`・列名なし）で残す

**残余リスク（設計上避けられないもの・記録）**:

- **フィールドの `id` の欠番**: `FieldDef.id` は外部の識別子（不変・採番は単調）なので詰めない。読めない列の分だけ
  `id` が飛び、「この位置に何か列がある」ことは分かる（名前・型・値は分からない）。外部の契約は参照名 `key` を
  正とし、`id` を表示や推測に使わないこと
- **テーブル名の重複（409 `name_taken`）**: テーブル名はテナント内で一意なので、見えないテーブルと同じ名前で
  作ろうとすると 409 になり、その名前のテーブルがあることが分かる（中身・権限は分からない）
- **見える unique 列の 409 `unique_violation`**: unique は**テーブル全体**の制約なので、見える列でも見えない行の値と
  重なれば 409 になり、その値を持つ行の存在が分かる（どの行かは分からない）。**行ポリシーのあるテーブルで unique を
  付けると、値の存在が他の編集者に分かる**（スキーマエディタで警告する）
- **単一値のリンク**: 見えない参照先を持つ行で単一値のリンクを置き換えると 403（見えない参照先を黙って外さない・
  PIT-20）、参照先が空なら 200 になり、見えない参照先の有無が分かる
- **マスク列だけの変更**でも `rev` と `updated_at` は進み、閲覧者の履歴には版の欠番が出る（PIT-70 の追補）
- **必須かつマスクの列**: 読めない人は作成も取込もできない（必須を満たせない）。スキーマ編集時に警告する

### 16.2 最適化層（AST があるから可能なもの）

> 統計・カーディナリティ推定・戦略とコストモデル・予算付きの切替・プランナ耐性 SQL・認可との関係は
> [query-optimizer.md](./data/query-optimizer.md) に分冊した。下表はその要約。

| 最適化 | 内容 |
|---|---|
| **戦略選択** | ソート駆動／フィルタ駆動／認可駆動（見える行が少ない利用者）／交差駆動／複合索引駆動から、統計と**閲覧者の認可材料の大きさ**でコストを見積もって選ぶ（PIT-69・query-optimizer §5） |
| **予算付きの切替** | 推定が外れて走査が伸びたら、次善の戦略の推定コストを予算として打ち切り、観測した選択率で選び直す（query-optimizer §6.2） |
| **述語の並べ替え** | 「(1 − 選択率) / 単価」の大きい順に検査する。順序は `CASE` の入れ子で固定する。**実装: 統計があるときにトップレベルの AND だけを選択率で並べる**（入れ子の項と統計の無いテーブルは葉の種類の静的順位。行述語の検査は常に最後） |
| **射影刈り込み** | `SELECT` に無い列を本体から取らない。**実装: 本体の JSONB から射影の列と事後検査（行述語の Rust 再評価）が読む列だけを DB 側で取り出す**（キー集合はバインド）。派生列・対称列を射影するもの（計算の材料に他の列が要る）と結合を残したクエリは刈り込まない |
| **JOIN の半結合化** | 結合先の列を `SELECT` にも `WHERE` にも使っていない場合、**認可述語つきの `EXISTS` へ変換する**（下記の注を必ず読むこと） |
| **恒真・恒偽の畳み込み** | `HasRole` はホスト側で解決済みなので TRUE/FALSE に潰れる。潰れた枝ごと消す |
| **プランキャッシュ** | 正規化したクエリ形状 → コンパイル済み SQL（§16.3） |

> ⚠️ **「JOIN 除去」と書いてはならない。** `INNER JOIN` は
> ①**参照先が無い行を結果から除外する** ②**多対一以外では行を重複させる**。
> 参照先の列を使っていなくても、**結合を単に消すと件数と集計値が変わる**。
> 正しい変換は**除去ではなく半結合化**であり、`EXISTS` には**参照先テーブルの行述語を必ず同じように
> 適用する**（これを落とすと PIT-20 の穴が最適化経路から開く）。
> 重複行の意味・カーディナリティ・集計への影響が安全と証明できない場合は、**変換せず結合を残す**。

### 16.3 プランキャッシュ / prepared statement 再利用

現在の `executor.rs` は `format!` で SQL を組み立てる（15 箇所）。問題は
**同じ論理クエリでもテキストが変わり得る**こと。

- Postgres は SQL テキストごとに parse / plan する
- **sqlx の prepared statement キャッシュもテキストがキー**
- テキストが毎回変われば全クエリが parse + plan を払う

v2 でのテキスト変動源は ①`HasRole` の定数畳み込み ②フィルタ / ソートの有無と形 ③射影の集合。

**対策は「変動をなくす」ではなく「変動を列挙する」。** クエリ**形状**（条件木の構造・ソートキー・
射影集合・畳み込み後の述語構造）をキーにし、**同一形状は必ずバイト同一のテキストを生成する**ことを
保証する。1 テーブル × 1 ビューあたりの形状数は小さいのでキャッシュは十分効く。

**キーに入れるもの / 入れてはならないもの**:

| 入れる | 入れない |
|---|---|
| 条件木の構造・ソートキー・射影集合 | **認可材料**（ロール集合・共有 id・principal ID） |
| `HasRole` の**畳み込み結果**（TRUE/FALSE で SQL 形状が変わるため） | `tenant_id` / `table_int`（**バインド値**なので形状に影響しない） |
| スキーマ由来の型・フィールド構造（`schema_version` 世代） | 値そのもの |
| コンパイラのバージョン（契約が変われば形状も変わる） | |

> **Phase 15 実装で変更**（`crates/data/src/query/planner/plan_cache.rs`・`COMPILER_VERSION = 2`）:
>
> - キャッシュの単位は **`(形状, 文の種類, 計画の変種)` → コンパイル済みの SQL テキストとバインドの種類の並び**。
>   文の種類は件数探索・候補・ソート駆動・値なしの区間・件数・集計・本体取得など、計画の変種は実行時に決まる構造
>   （戦略の入口・検査の並び・カーソルの有無・索引の状態・認可駆動の入口の形）で、値と認可材料は入らない
> - 命中したら降下でテキストを組まず、同じ走査で**バインドの値だけ**を集める（バインドの種類の並びが食い違えば組み直す。
>   検証ビルドでは命中のたびに組み直して一致を確かめる）。実測は [data/benchmarks.md](./data/benchmarks.md) §4
> - **`HasRole` の畳み込み結果はキーに入れない**。成否を真偽のバインド（スイッチ）で渡し、SQL テキストを分岐させないようにした
>   （テキストが分岐しないので、上表の「畳み込み結果を入れる」は不要になった。形状の数も材料の違いで増えない）
> - 値の長さ・材料の有無（共有集合が空か等）もバインド（配列・型つき `NULL`）で渡す

**認可材料をプランキャッシュへ混ぜてはならない。** 形状のキャッシュと材料は別物であり、
混ぜると他人の材料が載ったプランを引く事故になる。`tenant_id` / `table_int` はバインド値なので
キーに足す必要はない（足しても害はないがヒット率を下げる）。

> 見積もり: parse + plan は 1〜5ms 程度で、§16.4 の FGA 往復（10〜30ms）より小さいレバー。
> ただし**取るのが安く、形状を固定する設計は後から入れにくい**ので最初から入れる。

### 16.4 認可材料の解決コスト —— 最大のレバー

グリッド 1 ページの読取で、SQL を撃つ前に OpenFGA へ **2 往復**する
（`list_objects(Role)` ＋ `list_objects(DataRecord)`）。しかもグリッドは
「ページ＋件数＋lookup 解決」で複数クエリを撃つため、**素朴に実装すると 1 画面で 6 往復以上**になる。
p95 < 100ms の予算に対してこれは重い。

**PIT-18 の「キャッシュ禁止」はリクエストを跨いだキャッシュの話である。
同一リクエスト内のメモ化は安全**（1 リクエストの処理中に権限が変わる前提を置く必要はない）。

- **リクエストスコープのメモ化**で往復が 1/3 以下になる
- post-filter 用に **`AuthzClient::batch_check` を追加**する（現状 `try_join_all` で個別 check）

PIT-18 の不変条件を弱めずに取れる、最も費用対効果の高い改善である。

> **実装（Task 15.9・`crates/data/src/policy/memo.rs`）**: グリッド 1 画面（ページ＋件数＋lookup / rollup の解決）の OpenFGA 往復は
> **計 2 回**: ①テーブル viewer・参照先テーブルの viewer・`has_role(subtree=true)` の名指しロールの check をまとめた
> **BatchCheck 1 往復** ②個別共有の `list_objects(DataRecord)` 1 往復（`authz_memo_it::grid_screen_reads_with_at_most_two_fga_round_trips`）。
> `has_role(subtree=true)` は配下ロール込みの集合を展開せず名指しのロールだけを check で判定するので、`MAX_ROLE_SET` 超過のエラーに
> ならない。`has_role(subtree=false)` と `$user.roles` は従来どおり集合で引き、超過は fail-closed。

**memo のキー契約**（ここを緩めると PIT-18 を破ることになる）:

- キーには **`AuthContext` の関連部分を漏れなく含める**——最低でも
  `PrincipalKind` / principal ID / `org` / `tenant_id`。**加えて解決対象の式集合**
  （`material::resolve` は `&[&PolicyExpr]` を取るため、式が違えば材料も違う）
- **生存範囲はリクエストスコープに厳密に閉じる。** プロセス内・リクエスト跨ぎの保持は禁止
  （型で表現する。`Arc` などで request の外へ持ち出せない形にする）
- テストで固定する: 別テナント・別テーブル・別 principal で材料が混ざらないこと、
  **権限剥奪の次リクエストで即座に反映されること**

**プランキャッシュとは絶対に混ぜない**（§16.3）。プランキャッシュはクエリ形状のキャッシュであり、
認可材料はバインド値である。両者を同じキー空間に置くと、**他人の材料が載ったプランを引く**事故になる。

### 16.5 covering read（条件付き最適化）

表示列・フィルタ列・ソート列がすべて索引済みなら、値は索引テーブルに全部入っているので、
理論上 `data_record` に触れずに応答できる。**ただし無条件の得ではない。**

| | コスト |
|---|---|
| 本体を PK で 100 行引く | 索引 100 参照 ＋ heap fetch。**行が小さければ速い** |
| 索引テーブルから crosstab で組む | 100 行 × 5 列 = 500 索引行のグループ化。**それなりに重い** |

**分岐点は TOAST。** JSONB の `data` が TOAST 閾値（約 2KB）を超えると、キーを 1 つ読むつもりでも
**値全体が展開される**（JSONB はカラムナではないため、射影しても detoast は避けられない）。

→ **「本体が TOAST される幅のテーブルで、かつクエリが索引列だけで閉じているとき」に限り
covering read へ切り替える**。判定には平均行幅の統計が要る（§16.2 の統計基盤と共用）。

> ⚠️ **`truncated = true` の索引値を covering read に使ってはならない。**
> §3.3 のとおり索引には `MAX_INDEX_KEY_BYTES` のプレフィクスしか入っていない。
> これを完全なフィールド値として返すと、応答・比較・ソートのすべてが壊れる。
> **適用条件に「対象レコードのすべての射影列が `truncated = false`」を加える**。
> 1 行でも切り詰めがあれば、その行だけ `data_record` から取り直して完全な値で返す
> （ページ全体を諦める必要はない）。
>
> **「表示列を絞れば軽くなる」という直感は JSONB では成立しない**（転送量は減るが detoast は減らない）。
> 実装者が誤解しやすい点として明記する。

### 16.6 スキーマキャッシュ

`data_table.schema` は全クエリで引かれる。スキーマは権限ではないのでキャッシュ禁止の対象外であり、
プロセス内キャッシュを安全に置ける。地味だが全クエリに効く。

**キー設計に注意が要る。** `schema_version` は `data_table` の行ごとに `1` から始まる
（`migrations/0033_data_service.sql` の `default 1`）ため、**単独ではグローバルに一意ではない**。

- キーは **`(tenant_id, table_id, schema_version)`**（または衝突しないグローバル ID）
- スキーマ改訂・テーブル削除・**テナント purge** で**世代を原子的に切り替える**
  （削除・purge 後に古い世代が残って読まれない）
- キャッシュに載せるのは**スキーマ定義のみ**。認可材料は絶対に混ぜない（§16.4）

### 16.7 一括書込パス

10 万行インポート = 本体 10 万行 ＋ 索引 80 万行。1 行ずつ INSERT では成立しない。

> ⚠️ **COPY を authoritative table へ直接流してはならない。** 速いのは事実だが、それだけでは
> 通常の書込経路が担保している不変条件が**まとめて抜ける**——認可・unique・リンクのカーディナリティ・
> `rev`・outbox / SSE・材料化（`Invariant` 計算列）・索引 parity。型検証だけでは足りない。

**ステージング → 正規検証 → 適用**の 3 段にする。

```text
① 一時ステージング表へ COPY（型変換と整形のみ・authoritative には触れない）
② ステージング上で正規検証   … 通常書込と同じ検証器を通す
     認可（書込可否）・型・required・unique・リンクのカーディナリティ・多値要素数上限
③ チャンク Tx で適用         … 1 Tx で全件を抱えない（ロック保持時間を切る）
     本体 ＋ 索引行 ＋ revision ＋ outbox を通常経路と同じ規則で生成
     `Invariant` 計算列の材料化もここで走らせる
```

- **チャンク境界で冪等**にする（再実行が二重適用にならない。取込 ID ＋ 行キーで判定）
- **索引の遅延構築**は §10 の `building` 状態を流用する。**`building` 中も write-through する**ので、
  一括取込中の並行書込を取りこぼさない
- 適用後に**索引 parity 検証**を走らせる（本体と索引の突合）

> **Phase 15.2 のバックフィル自体がこの経路を要求する**ため、移行計画の前提として先に要る。

> **実装（Task 15.11・15.7・`migrations/0092`・`0096`・`crates/data/src/bulk/`）**:
>
> - ステージング `data_bulk_staging`（行番号・行キー・COPY 時に採番した外部 ID・`payload`・状態・正規化値）と進捗
>   `data_bulk_import`（`validating → applying → indexing → done`／`failed`）、取込内の一意性の予約 `data_bulk_key`。
>   主体はジョブで `AuthContext` を組み直し、**チャンクごとに editor を問い直す**（昇格しない）
> - **適用は 1 件の書込と同じ入口**（作成は `record_effects`、更新は `record_update` の行ロックつきの入口）を通るので、
>   本体・索引行・revision・**outbox（SSE の発火）**・材料化・監査が通常経路と同じ規則で出る。取込そのものは**新規作成**で、
>   既存レコードの更新を混ぜるのはワークフローの `data.bulk_upsert`（行ごとの `op` と任意の `expected_rev`）
> - 検証を通った後に並行書込と競合した行（unique・リンクのカーディナリティ）は拒否として数え、行エラーに残す。
>   `abort_on_invalid` なら 1 行でも不合格があれば何も適用しない。適用中にスキーマの版が変わったら未適用の行を検証し直す
> - 索引の構築は `index_mode`（`auto` / `immediate` / `deferred`）。`deferred` は**取込の作成行が値を書く列の**値索引を `building` に保持して
  後でバックフィルする（取込が値を書かない列の既存の索引は `active` のまま使える。null は索引行を持たないため）。
  取込・バックフィル・統計収集のジョブは可視期限を 60 秒に取り、処理中はハートビート（20 秒ごと）で延ばす（落ちたら 1 分以内に再配信）
> - **HTTP API**: `POST /data/tables/{id}/imports`（JSON の行）・`POST /data/tables/{id}/imports/csv`（`text/csv`・UTF-8・見出し行あり。
>   列は参照名→表示名の順で照合・`_key` 列は行キー・multi_select / record_ref は `;` 区切りか JSON 配列）で開始（202）。
>   進捗は `GET …/imports/{import_id}`、不合格・拒否の行は `GET …/imports/{import_id}/errors`（行番号の keyset）。
>   **Phase 15 で追補**: 進捗・行エラー（行の値の断片を含みうる）を読めるのは、テーブルの editor のうち**取込の作成者・テーブル owner・
>   組織管理者**だけ（他人の取込は「見つからない」）。読めない（マスクされた）列へ書く作成行を含む取込は開始時に 403、開始後に
>   書き手が読めなくなった列は検証段で行ごとに拒否する（行エラーに列名・値を出さない）
>   本文の上限は 32 MiB（超過は構造化 413 `import_too_large`）、行数の上限は 100 万行、入力の形の誤りは構造化 400 `import_invalid`
> - **10 万行の所定時間は 120 秒**（他テナントの p95 の悪化の許容は「平常 × 1.25 ＋ 2ms」）。根拠と実測（64〜77 秒）は
>   [data/benchmarks.md](./data/benchmarks.md) §3

### 16.8 共有プールの公平性

フルプールなので、1 テナントの重いクエリが他テナントを圧迫する。
`statement_timeout` は**上限であって公平性ではない**。

→ **テナント単位の同時実行数制限**を読取経路へ入れる
（workflow-engine のトークンバケット実装を再利用・design §4.12 の API レート制限と同じ枠）。

> **Phase 15 実装で変更**（Task 15.12・`crates/data/src/query/admission.rs`）: workflow のトークンバケットは使わず、
> **プロセス内（レプリカごと）・テナント別の同時実行の枠を、予測コストのトークン（ミリ秒）で消費する**形にした。
> 理由は 2 つ: ①トークンバケットは「時間あたりの回数」を数える仕組みで、重いクエリ 1 本と軽いクエリ 100 本を同じ物差しで
> 測れない（同時に走っている重さを測りたい） ②`shiki-workflow-engine` は `shiki-data` に依存しているので、逆向きの依存は作れない。
> 枠はテナントごとに独立しており（既定 8,000 トークン）、重いテナントのクエリ・統計収集はそのテナントの枠の中で待つ。
> 待ち時間（3 秒）を過ぎたら 429（`ReadOverloaded`・再試行可）。予測コストが読取予算（既定 2 秒）を超える見込みのクエリは
> そもそも実行しない（受付制御・[query-optimizer.md §5.4](./data/query-optimizer.md)）。全体の上限は `statement_timeout` と接続プールが担う。

### 16.9 残る未設計項目

| 項目 | 内容 |
|---|---|
| 集計の事前計算 | ダッシュボードの集計が毎回全走査。`Invariant` なテーブルに限り事前集計してよい（§7.4 と同じ規則が流用できる） |
| 件数クエリ | 上限付きでも 2 本目のクエリ。非同期化するか要求時のみに絞るか |
| lookup の N+1 | **方針決定**: 列ごとに 1 本の一括クエリ（`record_int` の集合で引く）＋ 1 ビュー 12 列まで（§8.2）。往復を 1 本へ畳むかは計測で決める |

### 16.10 優先順位

1. **§16.4（FGA 往復のメモ化）** — 取るのが安く、効果が最大
   （戦略選択の最小構成 O1 は読取切替の前提なので、優先順位の外で Phase 15.3 に入る。query-optimizer §10）
2. **§16.1 + §16.2（SQL 互換面と最適化層）** — チャット体験の質を決める。パーサは依存済み
3. **§16.3（形状固定とプランキャッシュ）** — 効果は中だが、後から入れにくいので最初に
4. **§16.6（スキーマキャッシュ）** — 全クエリへ効く
5. **§16.7（一括書込）** — Phase 15.2 のバックフィルが要求するので前倒し
6. **§16.5（covering read）** — 条件付き。統計基盤ができてから
