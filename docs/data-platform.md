# 構造化データ基盤 v2（Teable / Microsoft Lists 級）

> **本書は `crates/data`（構造化データサービス）の設計正本**。Phase 9 で実装した v1（[design.md §4.10](./design.md)）を
> 業務アプリ基盤として通用する水準へ引き上げる再設計を定める。実装順は [roadmap/phase-13.md](./roadmap/phase-13.md)。
>
> 関連正本: [design.md](./design.md)（全体構成・§4.1 テナンシー・§4.10 ミニアプリ基盤）/
> [requirements.md](./requirements.md)（FR-11・NFR-8）/ [miniapp-platform.md](./miniapp-platform.md)（ワークフロー・script・skill）/
> [design-caveats.md](./design-caveats.md)（PIT-17〜21＝v1 の行 authz 脅威モデル・**PIT-45〜48**＝v2 が持ち込む落とし穴）

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

```
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
| `table_id uuid` | `table_int` | `int8` | `data_table` 作成時にグローバル連番。**パーティションキー** |
| `field.name text` | `field_id` | `int2` | テーブル内で採番・不変・再利用しない。`0` は `owner` 擬似列に予約 |

> **`tenant_int` を索引テーブルにも持つ理由**: `table_int` はグローバル一意なので理論上は冗長だが、
> ①「全行 tenant スコープ・全クエリ tenant 条件付き」という day-1 不変条件（#91）を機械的に検査可能に保つ
> ②`purge_tenant` が結合なしで走る、の 2 点のため 4 バイトを払う。**冗長性は意図的**。

### 3.3 テーブル定義

```sql
-- ① 本体。形は v1 から変えない（JSONB のまま）。キーだけ "f{field_id}" 化する。
create table data_record (
    tenant_int int      not null,
    table_int  bigint   not null,
    id         uuid     not null,
    org        text     not null,
    data       jsonb    not null,   -- {"f1": "...", "f2": 12000}
    rev        bigint   not null default 1,
    owner      text     not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    primary key (tenant_int, table_int, id)
) partition by hash (table_int);    -- 64 分割（サイズ分割・autovacuum 局所化）

-- ② text 系索引（text / select / multi_select / date / datetime /
--    user_ref / role_ref / file_ref / status ＋ owner 擬似列 field_id=0）
create table data_index_text (
    tenant_int int      not null,
    table_int  bigint   not null,
    field_id   smallint not null,
    record_id  uuid     not null,
    ord        smallint not null default 0,  -- multi_select の要素番号
    val        text     not null collate "C",
    primary key (tenant_int, table_int, field_id, record_id, ord)
) partition by hash (table_int);
create index on data_index_text (tenant_int, table_int, field_id, val, record_id);

-- ③ 数値索引（number）。numeric = 正確な小数。
create table data_index_num (
    tenant_int int not null, table_int bigint not null, field_id smallint not null,
    record_id uuid not null, ord smallint not null default 0,
    val numeric not null,
    primary key (tenant_int, table_int, field_id, record_id, ord)
) partition by hash (table_int);
create index on data_index_num (tenant_int, table_int, field_id, val, record_id);

-- ④ リンク（record_ref）。順引きは PK、逆引きは専用索引。双方向リンクの実体。
create table data_link (
    tenant_int int not null, table_int bigint not null, field_id smallint not null,
    src_record uuid not null, ord smallint not null default 0,
    dst_table_int bigint not null, dst_record uuid not null,
    primary key (tenant_int, table_int, field_id, src_record, ord)
) partition by hash (table_int);
create index on data_link (tenant_int, dst_table_int, dst_record, table_int, field_id, src_record);

-- ⑤ unique 制約。1 本の PK で全テーブル・全フィールドの一意性を賄う。
create table data_unique (
    tenant_int int not null, table_int bigint not null, field_id smallint not null,
    val_norm text not null collate "C",
    record_id uuid not null,
    primary key (tenant_int, table_int, field_id, val_norm)
) partition by hash (table_int);

-- ⑥ 部分一致検索（オプトイン列のみ）。trigram GIN を「1 本だけ」張るための隔離先。
create table data_index_search (
    tenant_int int not null, table_int bigint not null, field_id smallint not null,
    record_id uuid not null, ord smallint not null default 0,
    val text not null,
    primary key (tenant_int, table_int, field_id, record_id, ord)
) partition by hash (table_int);
create index on data_index_search using gin (val gin_trgm_ops);
```

**索引の総本数はパーティションあたり 10 本の定数**（PK 6 本＝各テーブル 1 本ずつ、
値索引 2 本＝`text`/`num`、逆引き 1 本＝`link`、GIN 1 本＝`search`）。
**テナントが何社増えても、テーブルが何個増えても、列が何本増えても変わらない。**
（v1 は「テナント数 × テーブル数 × 索引列数」本が単一 relation に載る。10³ 社 × 10² テーブル × 5 列で 5×10⁵ 本。）

### 3.4 値の正規化

| 型 | 格納先 | 正規化 |
|---|---|---|
| `text` / `select` | `data_index_text` | そのまま |
| `multi_select` | `data_index_text` | **要素ごとに 1 行**（`ord` 昇順）。包含判定が等値検索になる |
| `number` | `data_index_num` | `numeric`（正確な小数） |
| `date` | `data_index_text` | `YYYY-MM-DD` 固定幅 → 辞書順＝日付順 |
| `date_time` | `data_index_text` | UTC 固定幅 ISO-8601 → 辞書順＝時刻順（v1 の正規化を踏襲） |
| `user_ref` / `role_ref` / `file_ref` | `data_index_text` | principal id / role id / node id |
| `record_ref`（リンク） | `data_link` | 参照先 `(dst_table_int, dst_record)`。多値は `ord` |
| `owner`（擬似列 `field_id=0`） | `data_index_text` | 全レコードに 1 行。`IsOwner` 述語を索引内で評価するため |

> **`COLLATE "C"` を使う理由**: バイト順の決定性を得るため。ICU ロケール照合は collation version が
> 上がると既存索引が壊れる（`pg_collation` バージョン不一致）リスクがあり、索引テーブルには不適。
> **代償**: 漢字の並び順は符号位置順になり、日本語として正しくない。
> → **読み仮名フィールドを別に持ち、それでソートする**のが正解（Excel/kintone も同じ運用）。
> ビューのソート設定 UI で「この列の並べ替えには読み列を使う」を指定できるようにする（§8）。

### 3.5 クエリの形

**「状態=承認待ち の申請を、金額が高い順に 20 件」**

```sql
WITH page AS (
  SELECT i.record_id, i.val
  FROM data_index_num i                                    -- 駆動＝ソートキーの索引（既に金額順）
  WHERE i.tenant_int = $1 AND i.table_int = $2 AND i.field_id = $3   -- f2 = 金額
    AND (i.val, i.record_id) < ($cursor_val, $cursor_id)             -- keyset（DESC）
    AND EXISTS (SELECT 1 FROM data_index_text f                      -- フィルタ＝半結合
                WHERE f.tenant_int = $1 AND f.table_int = $2
                  AND f.field_id = $4 AND f.val = '承認待ち'
                  AND f.record_id = i.record_id)
    AND ( <行述語も索引テーブル上で評価。§6.2> )
  ORDER BY i.val DESC, i.record_id DESC
  LIMIT $n
)
SELECT r.* FROM data_record r
  JOIN page p ON r.tenant_int = $1 AND r.table_int = $2 AND r.id = p.record_id
ORDER BY ...;
```

- **駆動索引はソートキー**が既定。`LIMIT` で早期終了するため、**総行数に依存しない**
- フィルタは `EXISTS` 半結合。複数条件は AND で連鎖
- 可視でない行は**本体に触れる前に落ちる**
- 本体取得は PK 直接アクセス 20 回

### 3.6 書き込みの形

1 トランザクションで:

```
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
- **多値列（multi_select / link）の要素数に上限**を設ける（1 レコードが数千行に膨らむのを防ぐ・→ **PIT-48**）
- パーティション単位のアーカイブ／退避

### 3.8 パーティション

`PARTITION BY HASH (table_int)` を 64 分割。クエリは常に `tenant_int = $1 AND table_int = $2` を
持つため、**必ず 1 パーティションに枝刈りされる**。

- サイズ分割と autovacuum の局所化が目的（索引本数の削減が目的ではない——それは §3.3 が既に解決している）
- 分割数はプロビジョニング時の設定値。増やす場合は再ハッシュが要るため、初期値は余裕をもって決める
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
}
```

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

```
既存 name = "applicant"
  → id = 宣言順に 1 から採番
  → key = "applicant"（そのまま）
  → display_name = "applicant"（後からユーザーが日本語へ変更）
JSONB: {"applicant": "佐藤"} → {"f1": "佐藤"}
```

---

## 5. クエリ IR

### 5.1 型

```rust
pub struct DataQuery {
    pub filter: Option<Condition>,   // AND/OR/NOT の条件木
    pub sort: Vec<SortKey>,          // 複数キー
    pub page: Page,                  // Cursor(keyset) | First(n)  ← OFFSET は廃止
    pub group_by: Vec<FieldRef>,
    pub aggregate: Vec<Metric>,
    pub select: Option<Vec<FieldRef>>, // 投影（グリッドは可視列だけ取る）
    pub search: Option<String>,        // 横断部分一致（searchable 列のみ）
}

pub enum Condition {
    All(Vec<Condition>),
    Any(Vec<Condition>),
    Not(Box<Condition>),
    Cmp { field: FieldRef, op: CmpOp, value: Operand },
}

pub enum CmpOp {
    Eq, Neq, Lt, Lte, Gt, Gte, In,
    Contains, StartsWith, EndsWith,   // 文字列
    Exists, IsNull,                   // 有無
}
```

**演算子語彙は workflow IR の `Condition`（`crates/workflow-engine/src/ir/`）に合わせる。**
そうすることで `data.query` ノードは IR 条件を**変換なしで**渡せる。
`matches`（正規表現）は索引が効かず DoS 面にもなるため**採用しない**（IR 側にはあるが data では拒否）。

> **`Not` の扱い**: `row_policy`（`PolicyExpr`）は意図的に `Not` を持たない（フィールドマスクとの
> 相互作用で情報が漏れるため・`crates/data/src/policy/ast.rs`）。**クエリフィルタは別物なので `Not` を許す**。
> マスク列はそもそもフィルタに出せない（§6.3）ため、否定による漏洩経路は生じない。この非対称は意図的である。

### 5.2 索引到達可能性の強制

v1 の `indexed_queryable` を継承・強化する。

| 制約 | 理由 |
|---|---|
| `filter` / `sort` / `group_by` / `aggregate` の対象は `indexed \|\| unique` 宣言済みのみ | 索引テーブルに載っていない列は引けない |
| マスク対象フィールドは上記いずれにも出せない（403） | PIT-19（表示を隠しても並べ替えで漏れる） |
| `Contains` / `EndsWith` は `searchable` 宣言済みのみ | trigram GIN（`data_index_search`）を要する |
| 条件木に**索引可能な述語を最低 1 つ**含むこと | 駆動索引が選べない全走査クエリを API から作れなくする |
| `statement_timeout` を data 経路に設定 | 想定外プランの保険 |

**索引未宣言の列を指定されたら 403 を返しつつ、自動昇格（§10）を提案する**のが UX 上の解。

### 5.3 ページング

```rust
pub enum Page {
    First { limit: u32 },
    Cursor { after: Cursor, limit: u32 },
}
```

カーソルは **全ソートキーの値 ＋ record_id** を不透明トークンにしたもの。
`(val₁, val₂, …, record_id)` のタプル比較で継続位置へ直接飛ぶ。**何ページ目でも同じコスト**。

> フロントの `web/src/hooks/use-infinite-list.ts` は既に `next_cursor` 前提の実装なので噛み合う。
> v1 の OFFSET（clamp 10,000）はこのフックとインピーダンス不整合だった。

### 5.4 件数

10⁷ 行で「条件に合う可視行の正確な件数」は成立しない（全行の権限判定が必要）。

```rust
pub enum CountResult {
    Exact(u64),              // 上限内
    AtLeast(u64),            // "10,000+"
}
```

`LIMIT 10001` のサブクエリで数え、10001 なら `AtLeast(10_000)`。
Airtable / Teable / SharePoint も同種の妥協をしている（SharePoint の 5,000 件ビュー閾値が典型）。
**上限値はテナント設定で変更可能**にする（小さいテーブルしか持たない顧客には正確な件数を出せる）。

### 5.5 集計

v1 の `Metric { Count, Sum, Avg, Min, Max }` と `Aggregate { group_by, metric, field }` を拡張。

- 数値集計は `data_index_num` の `val numeric` で計算（**正確な小数**）
- グループ化は `data_index_text` の値ブロックで区切る
- **可視行に限る**（行述語を索引内で交差）
- **スモールセル抑制（`DEFAULT_AGGREGATE_MIN_ROWS = 5`）を v1 のまま継承**（PIT-17）
- 集計クエリ自体の監査記録も継承（反復差分攻撃の検知可能性）

---

## 6. 認可（4 階層の維持）

**v1 の 4 階層をそのまま維持する。変えるのは述語の評価場所だけ。**

| 階層 | v1 | v2 |
|---|---|---|
| ① テーブル ReBAC | OpenFGA `data_table` viewer/editor/owner | **変更なし** |
| ② 行述語 ABAC | `PolicyExpr` → `WHERE` 断片へコンパイル・値は必ずバインド | **同じ AST・同じ材料解決。出力先が索引テーブル条件になる** |
| ③ フィールドマスク | `field_policy` で応答から除去＋filter/sort から 403 | **変更なし**（§5.2 に継承） |
| ④ 行個別共有 | `data_record` スパースタプル（`= ANY($n::uuid[])`） | **変更なし**（`record_id` に直接効く） |

### 6.1 材料解決

`crates/data/src/policy/material.rs` をそのまま使う。**キャッシュ禁止の原則も維持**
（権限剥奪・ロール変更・共有解除の即時反映のため）。`MAX_ROLE_SET = 1000` 超過は fail-closed、
`MAX_SHARED_IDS = 10_000` 超過は切り詰め＋`shares_truncated` 通知（可視が減る方向）も継承。

**例外はリアルタイム配信のみ**（§9.4 で TTL 付き再解決を明示の約束として定義する）。

### 6.2 行述語を索引テーブルで評価する

行ポリシー「自分の申請、または自分の部門の申請」は、実行者が佐藤（営業部）のとき次に畳み込まれる
（`HasRole` はホスト側で解決済みの定数になる・v1 の挙動）。

```
(f1 = "佐藤") OR (f5 IN ("営業"))          ← f1=申請者, f5=部門
```

`f1` も `f5` も索引テーブルに載っているので、**この判定が索引内で完結する**。

```sql
AND ( EXISTS (SELECT 1 FROM data_index_text p WHERE p.tenant_int=$1 AND p.table_int=$2
                AND p.field_id = 1 AND p.val = $owner_id AND p.record_id = i.record_id)
   OR EXISTS (SELECT 1 FROM data_index_text p WHERE p.tenant_int=$1 AND p.table_int=$2
                AND p.field_id = 5 AND p.val = ANY($dept_array) AND p.record_id = i.record_id)
   OR i.record_id = ANY($shared_ids::uuid[]) )                    -- ④ 個別共有
```

- `IsOwner` は `field_id = 0`（owner 擬似列）で同じ形になる
- `Public` は定数 TRUE、`HasRole` は定数 TRUE/FALSE に畳み込み（v1 と同じ）

### 6.3 新規制約: `row_policy` 参照フィールドは `indexed` 必須

**スキーマ保存時に検証し、違反は 422 で拒否する。**

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

---

## 7. リンク・ルックアップ・ロールアップ・数式

### 7.1 リンク（双方向）

`data_link` の 1 テーブルで両方向を賄う（§3.3 の逆引き索引）。

```
「proj_A の顧客は？」  → PK で (table=案件, field=f9, src=proj_A) → dst=cust_001
「cust_001 の案件は？」→ 逆引き索引で (dst_table=顧客, dst=cust_001) → proj_A, proj_B
```

**逆参照のために別の索引を用意する必要がない。** これが双方向リンク（1:1 / 1:N / N:N）を
実装する上での本方式の最大の利点。

- **N:N** は `ord` による多値で表現（junction テーブル不要）
- **対称フィールド**（参照先テーブルに自動生成される逆リンク列）はスキーマ上のメタデータであり、
  実体は同じ `data_link` 行を逆から読むだけ。**二重書き込みをしない**（不整合が構造的に発生しない）
- 参照整合性は書込時にサーバが検証（v1 の `validate.rs` を継承）

### 7.2 ルックアップ

参照先の列の値を引いて表示する。

```
① 案件を 20 件取得（f9 に顧客 ID が入っている）
② その顧客 ID 20 件で、顧客テーブルの索引/本体を 1 クエリで引く
   ← このとき参照先テーブルの行述語を「閲覧者本人の権限で」適用する
③ 見えない顧客の値は null（存在秘匿）
```

v1 の `select_lookup_values` が既にこの形。**バッチ取得（フィールドごとに 1 クエリ）を維持**する。

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

- **依存 DAG をスキーマ保存時に構築し、循環を検出して 422**（`MAX_FORMULA_DEPTH` も設ける）
- 算術は `numeric`（金額の正確性）
- LLM / 開発者由来の任意式が SQL になる経路を作らない（v1 の設計原則を継承）

### 7.4 マテリアライズ可否 — **authz 不変性で決める**

**これが本章で最も重要な規則。後から入れることが構造的に不可能なので、最初から型に入れる。**

ロールアップの合計値を書込時に計算して保存すると、**閲覧者ごとに違うはずの値が同じになる**。

```
経費テーブルに 10 件（合計 100 万円）
  部長には 10 件全部見える     → 合計 100 万円
  一般社員には 3 件だけ見える   → 合計 30 万円であるべき
  ↑ 保存してしまうと一般社員にも 100 万円が見え、見えない 7 件の情報が漏れる
```

```rust
pub enum Materialization {
    /// 参照先テーブルに row_policy も field_policy も無い
    /// → 全閲覧者で同値 → 書込時に材料化・索引可・ソート/フィルタ可
    Invariant,
    /// どちらかがある → 閲覧者ごとに値が変わる
    /// → 読取時計算のみ・キャッシュ禁止・索引不可・ソート/フィルタ/集計の対象にできない
    PerViewer,
}
```

- **スキーマ保存時に参照先を推移的に辿って判定する**（多段参照は 1 つでも policy があれば `PerViewer`）
- `PerViewer` なフィールドへの `indexed` / `unique` 宣言は**拒否**（422）
- 参照先に後から `row_policy` を付けたら、**依存する計算列の材料化データを破棄して `PerViewer` へ降格する**
  （スキーマ改訂時の再評価。これを忘れると漏洩が残留する → **PIT-47**）
- ロールアップのスモールセル抑制は参照先の `aggregate_min_rows` を継承

---

## 8. ビュー（ViewSpec）

### 8.1 v1 の問題

`DataViewBody.display` が `serde_json::Value` でサーバ非解釈（`crates/data/src/view.rs`）。
ビュー種別も列設定も無く、フロントが解釈規約を持てない。

### 8.2 設計

**Rust 型を単一ソースとし、ts-rs / utoipa で TypeScript を生成**（「codegen が正・手書き型を作らない」）。

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
    pub columns: Vec<ColumnConfig>,   // 表示順・幅・固定・非表示
    pub row_height: RowHeight,
    pub group_by: Vec<GroupConfig>,   // 行グループ化（折りたたみ）
    pub conditional_format: Vec<ConditionalFormat>, // 条件付き書式（閉じた条件木）
    pub sort_override: Option<Vec<SortKey>>,
}

pub struct ColumnConfig {
    pub field: FieldId,
    pub width: Option<u16>,
    pub hidden: bool,
    pub frozen: bool,
    /// 並べ替えに使う代替列（日本語の読み仮名列など・§3.4）
    pub sort_by: Option<FieldId>,
}
```

- **サーバが検証する**: 存在しない `field_id`・マスク列の表示指定・`PerViewer` 列でのソート指定を拒否
- 保存は v1 どおり `artifact(kind=data_view)` の枠に乗せ、ReBAC 共有と不変バージョンを継承
- **実行は必ず `run_query` を経由**し、**閲覧者本人の権限で毎回再評価**（作成者の権限を引き継がない・v1 の不変条件）

---

## 9. リアルタイム配信

### 9.1 v1 の欠落

`create_record` / `update_record` / `delete_record` は**イベントを出していない**
（outbox を出すのは FSM 遷移のみ・`crates/data/src/transition.rs`）。

### 9.2 変更ログと順序

**既存の outbox ＋ per-consumer fan-out リレー（`crates/storage/src/event.rs`）を再利用する。**

- 書込トランザクション内で outbox に 1 行（`data.record.upserted` / `data.record.deleted`）
- **リレーがコミット順に排出し、配信シーケンスを採番する**
  （DB シーケンスを直に使うと「seq=5 が seq=7 より後にコミットする」ため購読者が取りこぼす。
  この問題は既存のリレー実装が既に解いている）
- `data_record_revision` は追記型なので、`Last-Event-ID` からのリプレイ元として使える

### 9.3 配信

```
GET /data/tables/{table_id}/stream?since=<seq>     (SSE)
  event: record.upserted   id=<seq>   data={record_id, rev, fields...}
  event: record.deleted    id=<seq>   data={record_id}
  event: schema.changed    id=<seq>
```

**購読者ごとに行述語とフィールドマスクを再評価してから配信する。**
見えない行・見えない列は物理的にストリームに乗らない。

### 9.4 ファンアウトのスケール

素朴には 1,000 購読者 × 100 更新/秒 = 10 万回/秒の判定になり破綻する。2 つの工夫で抑える。

1. **250 ms 窓でバッチ**（1 件ずつ送らない）
2. **述語グループ化**: `compile_read_predicate` は `HasRole` をホスト側で解決して定数へ畳み込むため、
   **同じロール構成のユーザーは同一の SQL 断片＋バインド値になる**。
   その**ハッシュで購読者を束ね、グループごとに 1 回だけ評価**する。1,000 人が 3 ロールなら評価は 3 回

**権限材料の再解決について（明示の製品約束）**:
材料解決は毎回 OpenFGA を叩く仕様（PIT-18・キャッシュ禁止）だが、SSE で毎更新ごとに叩くと OpenFGA が持たない。
→ **購読中は TTL（既定 5 秒）で再解決する。したがって権限剥奪の配信への反映は最大 TTL 分遅れる。**
これは設計上の妥協であり、**製品の約束として文書化する**（→ PIT-46）。
ロール変更・共有解除・テーブル権限剥奪のイベントを検知した購読は TTL を待たず即時無効化する。
なお **REST の読取経路は従来どおり毎回解決**であり、遅延は SSE 配信にのみ適用される。

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

```
ユーザーが「備考」列でフィルタしようとする
  → 未索引 → 403 ではなく「この列での絞り込みを有効にしますか？」を提示
  → 小さいテーブル（閾値以下）は即座にバックフィルして透過的に有効化
  → 大きいテーブルは jobq でバックフィル（進捗表示・中断再開可）→ 完了後に有効化
```

- バックフィルは `crates/jobq` のバッチジョブ。**チャンク単位・冪等・中断再開可**
- 進行中は当該列を「準備中」として扱い、クエリには使わせない（**半端な索引で結果を欠落させない**）
- 逆方向（一定期間使われない索引の降格）も同じ機構で行える
- **テーブルあたりの索引列数に上限**を設け、超過時は明示承認を要求する（無自覚な肥大化を防ぐ・→ **PIT-48**）
- v1 の `CREATE INDEX` 方式ではこの挙動は危険で選べなかった

---

## 11. 各面からの参照

**設計の芯: 各面に個別実装を散らさず、単一の `DataQuery` / `ViewSpec` を全面が共有する。**
`workflow-engine::vocab::catalog` が「UI パレット・右パネル・AI ツール description の共通ソース」に
なっている先例と同じ流儀を data にも適用する。

| 面 | 現状 | v2 |
|---|---|---|
| **app-gateway**（B1/B2 ミニアプリ） | ✅ 9 ルート実装済み | クエリ IR 刷新に追従。`transition` を script からも使えるようにする |
| **shiki script（B2 関数）** | ✅ `Shiki.data.*`（gateway 経由） | 同上 |
| **shiki script（ワークフロー/skill）** | ❌ `HostBridge` が `data.*` を受けない | **`nodes/script.rs` の dispatch に data アームを追加**。api 名は `ALLOWED_APIS` に既存 |
| **ワークフローノード** | ❌ 語彙予約のみ・`available_stage_a` false | **`data.query` / `data.record.create` / `.update` / `.delete` / `.transition` / `.bulk_upsert` を解禁**。IR `Condition` と `DataQuery.filter` が同語彙なので変換層が不要。書込は `effect_journal`（EngineDedup）経由 |
| **generative UI** | ❌ `TableProps` は静的データ埋め込み | **`ActionBinding` に第 4 種 `DataView { view_id, pinned_version }` を追加**し、`data_grid` コンポーネントを束縛で描画。クライアントが送れるのは `action_id + params` のみ（`ActionDispatcher` の不変条件を維持） |
| **chat / agent-core** | ❌ data 系ツールがゼロ | **`data_query`（読取）・`data_record_write`（`requires_confirmation`）・`data_schema`（グラウンディング）**。`csv_tool.rs` の 3 ツールが最も近い先例。description には**発話ユーザーが viewer のテーブル一覧**を動的に載せる（`SkillTool` と同じ流儀・テーブル数は有界） |
| **RAG** | ❌ 構造化データのフックがゼロ | **§11.1** |

### 11.1 RAG 統合（方向性）

「権限考慮 RAG プラットフォーム」として最も効く統合であり、**本方式ときれいに嵌まる**。

- テーブル単位で「検索対象にする」をオプトイン。レコード 1 件 = 1 チャンク。投影テンプレート（タイトル列・本文列）を指定
- **pre-filter**: `data_table` を authz タグに使う（既存の folder/file タグと同型）
- **post-filter**: 行の可視性は SQL 述語なので、**候補 record_id を可視集合に絞るクエリ 1 本**で済む。
  **OpenFGA のタプルを 1 本も増やさずに行レベルの permission-aware 検索が成立する**
  （`crates/rag/src/authz_filter.rs` の file 粒度 post-filter を多型化する）
- 増分索引は §9.2 の outbox に相乗り

v1 スコープでは**設計スパイクまで**（実装はポストアルファ可・Phase 13.8）。

---

## 12. スケールアウト戦略（4 段の階段）

フルプールを選んだ以上、「顧客が増えたらどうするか」を設計時点で答えておく。

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
- **シャード安全の禁止事項リスト**を明文化し、CI で検査する:
  - テナント横断の JOIN を書かない
  - グローバル連番に依存しない（`table_int` は採番サービス経由・シャード跨ぎで衝突しないこと）
  - テナント横断の集計は「各プールへ問い合わせて合算」する形にする（単一クエリで書かない）
  - 外部キーがテナント境界を跨がない
- Notion の「Postgres 単一 → テナント単位シャード」の移行が先例

### 第 3 段: クジラ顧客の専用棟（= cell オプションの正体）

- 巨大顧客 1 社を専用プールへ移す。**これが cell の実体**であり、
  「最初から全顧客を cell にする」のとは経済性がまったく違う
- 引越は `tenant_int` 単位の行コピー＋レジストリ切替。既存の `shiki-admin retenant`（#89）が原型

### 12.1 残課題（正直に記録する）

| 課題 | 状況 |
|---|---|
| 引越中の整合（無停止移行） | 未設計。読取専用期間を許すか、二重書込＋切替かの判断が要る |
| Keycloak / OpenFGA 共有プレーンのスケール | データプレーンを割っても identity は共有のまま（PIT-26）。OpenFGA の水平分割は別課題 |
| テナント単位 PITR | フルプールでは物理 PITR で 1 社だけ戻せない。論理エクスポート/インポートによる部分復旧を用意する（design §4.12・Phase 12.10） |
| `AuthzClient` に BatchCheck が無い | 現状 `try_join_all` で個別 check。OpenFGA の `/batch-check` を追加すべき |

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

---

## 14. 運用

- **autovacuum**: 索引テーブルは更新が多い。パーティション単位で `autovacuum_vacuum_scale_factor` を下げる
- **膨張監視**: `pgstattuple` でパーティションごとの死行率を定点観測。閾値超過で `REINDEX CONCURRENTLY`
- **EXPLAIN 回帰を CI ゲートに**: 代表クエリ集合について実行計画（駆動索引・Index Scan の使用）を
  アサートする。`crates/data/tests/data_it.rs` が既に EXPLAIN で索引使用を検証している前例を拡張する。
  **pivot 方式は駆動索引の選択を誤ると 100 倍遅くなる**ため、これは受け入れ条件に含める（→ **PIT-45**）
- **統計**: フィールド単位の粗いカーディナリティ統計をレジストリに持ち、背景ジョブで更新。
  クエリコンパイラの駆動索引選択に使う（Salesforce が自前オプティマイザを持つのと同じ理由）
- **バックフィルの可観測性**: 索引昇格ジョブの進捗・残件・ETA を管理画面に出す
- **多値上限**: `multi_select` / `link` の 1 レコードあたり要素数に上限（既定値を設定可能に）

---

## 15. 将来オプション・スコープ外

| 項目 | 位置づけ |
|---|---|
| **フォームビュー（外部提出）** | 未認証提出は authz 的に別物（誰の権限で書くか）。v1 スコープ外・別途設計 |
| **添付ファイル欄・レコードコメント・通知センター** | Lists 相当機能。v1 スコープ外 |
| **列指向の分析スナップショット** | 1,000 万行の集計は行 DB が苦手。**正本は Postgres、分析ビューだけテーブル単位のスナップショットを DuckDB で持つ**。`crates/tabular`（CSV 分析に隔離 DuckDB を使用）という前例があり、**既存アーキテクチャに足すだけで何も壊れない** |
| **専用ストア（cell）／SQLite per tenant** | §12 第 3 段。強い隔離が要件化した顧客向け。**そのために物理層を Postgres 固有の飛び道具に依存させない**（本設計は JSONB・`numeric`・btree・GIN という一般的な機能しか使っていない） |
| **テナント引越ツール・実シャーディング** | §12 第 2 段。継ぎ目のみ先行 |
| **正規表現フィルタ（`matches`）** | 索引が効かず DoS 面。採用しない |
