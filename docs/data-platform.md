# 構造化データ基盤 v2（Teable / Microsoft Lists 級）

> **本書は `crates/data`（構造化データサービス）の設計正本**。Phase 9 で実装した v1（[design.md §4.10](./design.md)）を
> 業務アプリ基盤として通用する水準へ引き上げる再設計を定める。実装順は [roadmap/phase-13.md](./roadmap/phase-13.md)。
>
> 関連正本: [design.md](./design.md)（全体構成・§4.1 テナンシー・§4.10 ミニアプリ基盤）/
> [requirements.md](./requirements.md)（FR-11・NFR-8）/ [miniapp-platform.md](./miniapp-platform.md)（ワークフロー・script・skill）/
> [design-caveats.md](./design-caveats.md)（PIT-17〜21＝v1 の行 authz 脅威モデル・**PIT-45〜50**＝v2 が持ち込む落とし穴）

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
| `table_id uuid` | `table_int` | `int8` | `data_table` 作成時にグローバル連番。**パーティションキー** |
| `field.name text` | `field_id` | `int2` | テーブル内で採番・不変・再利用しない。`0..15` はシステム擬似列に予約 |

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
--    user_ref / role_ref / file_ref / status ＋ システム擬似列 field_id 0..2）
--    val は「索引キー」であって値の正本ではない（正本は data_record.data）。
--    btree キー上限（1 ページの 1/3 ≒ 2704B）を超えないよう MAX_INDEX_KEY_BYTES で切り詰める。
create table data_index_text (
    tenant_int int      not null,
    table_int  bigint   not null,
    field_id   smallint not null,
    record_id  uuid     not null,
    ord        smallint not null default 0,  -- multi_select の要素番号
    val        text     not null collate "C",-- 先頭 MAX_INDEX_KEY_BYTES バイトのプレフィクス
    truncated  boolean  not null default false, -- true なら等値比較は本体で再確認する
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

-- ④ リンク（record_ref）。1 エッジにつき「順方向」「逆方向」の 2 行を書く（下の注を参照）。
--    どちらの行も table_int = 自分が所属する側のテーブルなので、両方向でパーティション枝刈りが効く。
create table data_link (
    tenant_int int not null,
    table_int  bigint not null,       -- この行が所属する側のテーブル（＝パーティションキー）
    field_id   smallint not null,     -- 順方向はリンク列、逆方向は対称列の field_id
    src_record uuid not null,         -- この行が所属する側のレコード
    ord        smallint not null default 0,
    dst_table_int bigint not null,    -- 相手側
    dst_record uuid not null,
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
    record_id uuid not null,
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
    record_id uuid not null, ord smallint not null default 0,
    val text not null,
    primary key (tenant_int, table_int, field_id, record_id, ord)
) partition by hash (table_int);
-- スコープ列を GIN 自体に含める（btree_gin）。含めないと 1 テナントの検索でも
-- プール全体の posting list を読み、ノイジーネイバーが p95 を壊す。
create index on data_index_search
    using gin (tenant_int, table_int, field_id, val gin_trgm_ops);
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

**切り詰め値でのソートと keyset の契約**（曖昧にすると重複・欠落が出るので明文化する）:

- ソートは**プレフィクス順で走査する**。同一プレフィクス群の中の順序は**完全値順を保証しない**
- ただし **keyset は `(val_prefix, record_id)` のタプル比較で進む**ため、
  **ページ間の重複・欠落は発生しない**（順序が「完全値順ではない」だけで、全順序としては決定的）。
  `record_id` が常にタイブレーカに入ることがこの保証の根拠
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
- **ライブページングでは「途中で行が消えた／二重に出た」をクライアントが検知できるよう、
  各ページに `snapshot_seq` を添えて返す**。グリッドはこれと SSE の `delivery_seq` を突き合わせて
  自前で整合を取る（再読込を強制しない）

**索引の総本数はパーティションあたり 10 本の定数**（PK 6 本＝各テーブル 1 本ずつ、
値索引 2 本＝`text`/`num`、GIN 1 本＝`search`、リンクのカーディナリティ強制用の部分一意 1 本。
`link` は 2 行方式にしたため逆引き専用索引は不要）。`data_link_field` は
**リンク列ごとに 1 行**の小さなメタデータ表で、レコード数には比例しない。
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

- フィルタは `EXISTS` 半結合。複数条件は AND で連鎖
- 可視でない行は**本体に触れる前に落ちる**
- 本体取得は PK 直接アクセス 20 回

#### 駆動索引の選び方（null 落ちと初期表示の両方がここに掛かる）

pivot 索引は**値が無いレコードには行が無い**。ソートキーの索引を素朴に駆動表にすると、
**そのフィールドが未設定の行が結果から丸ごと消える**（`NULLS LAST` に並ぶのではなく欠落する）。
任意列でのグリッドソートは通常操作なので、これは可視行の欠落＝バグになる。

そこで駆動索引を次の規則で選ぶ。

| 状況 | 駆動 | 早期終了 |
|---|---|---|
| ソートキーが `required`（全行に値がある） | そのフィールドの索引 | ✅ 効く |
| ソートキーが任意（null あり） | **システム擬似列**（`created_at`=1 等・全行に存在）を駆動にし、ソート値を `LEFT JOIN` して `NULLS LAST` | ❌ 効かない（後述の緩和） |
| ソート指定なし（初期グリッド） | `updated_at`（擬似列 2）の索引 | ✅ 効く |
| 選択的フィルタがある | そのフィルタの索引を駆動にしてソートは後段 | 統計で判断（§14） |

**システム擬似列（`owner` / `created_at` / `updated_at`）は全レコードに必ず 1 行あるため、
「全件を走査できる索引」として常に使える。** これが null 落ちと「フィルタなしの初期グリッド」の
両方を解く鍵になる。

任意列ソートで早期終了が効かない件の緩和: ①ビュー設定の既定ソートにはシステム列を使う
②任意列を既定ソートにしたい場合は当該列を `required` にするか、**索引の自動昇格（§10）で
「null センチネル行」を含めて材料化する**（未設定レコードにも `val = ''`・`is_null = true` の行を持たせる）。
どちらを採るかは列ごとにスキーマで宣言する。

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
    /// number 型のみ: 小数桁数と範囲。表示・丸めの規則を決め、
    /// 併せて将来の物理層差し替え時の変換を機械的にする（§15.1 の保険③）。
    pub numeric_spec: Option<NumericSpec>,
}

/// `number` フィールドの精度宣言。
pub struct NumericSpec {
    /// 小数桁数（0..=9）。保存値の正規化・表示・入力検証に使う。
    pub scale: u8,
    /// scale を超える桁が来たときの扱い。
    pub on_excess_scale: ExcessScale,
    /// 許容範囲（省略時は型の上限）。**正規化後の値**に対して検証する。
    pub min: Option<Decimal>,
    pub max: Option<Decimal>,
}

pub enum ExcessScale {
    /// 422 で拒否する（既定。入力ミスを黙って変えない）
    Reject,
    /// 指定の丸めモードで丸める
    Round(RoundingMode),
}

/// 丸めモード（曖昧さを残さないため明示列挙）。
pub enum RoundingMode { HalfUp, HalfEven, Down, Up }
```

**正規化規則を固定する**（未定義のままだと保存値・pivot 索引・集計・将来の変換で結果がずれる）。

1. 書込時、値を **`scale` へ正規化**する。`on_excess_scale = Reject` なら
   scale を超える桁がある入力は **422**（既定。黙って値を変えない）。
   `Round(mode)` なら宣言したモードで丸める
2. **`min` / `max` の検証は正規化の後**に行う（丸めで境界を跨ぐケースの挙動を一意にする）
3. **本体・索引・集計はすべて正規化後の同一値を使う**（本体と索引で丸めが違う事故を防ぐ）

**スキーマ検証**（`NumericSpec` 自体の妥当性）:

- `field_type != Number` に `numeric_spec` が付いていたら 422
- `min > max` は 422
- `scale` が範囲外（`> 9`）は 422

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
| **駆動索引が 1 つ以上決まること**（索引済みフィルタ条件／`required` なソートキー／システム擬似列のいずれか） | 全走査クエリを API から作れなくする。**システム擬似列は常に駆動になれるので「フィルタなしの初期グリッド」は成立する** |
| `statement_timeout` を data 経路に設定 | 想定外プランの保険 |

> 「条件木に索引可能な述語を最低 1 つ」という強い形にはしない。`filter = None` の初期グリッド
> （テーブルを開いた直後の 100 行表示・Phase 13.5 の要件）が常に拒否されてしまうため。
> 代わりに**駆動索引の存在**を条件とし、システム擬似列（全行に存在）を既定の駆動として認める。

**索引未宣言の列を指定されたら 403 を返しつつ、自動昇格（§10）を提案する**のが UX 上の解。

### 5.3 ページング

```rust
pub enum Page {
    First { limit: u32 },
    Cursor { after: Cursor, limit: u32 },
}
```

カーソルは **全ソートキーの値 ＋ record_id** を不透明トークンにしたもの。

**単一ソートキーの場合**は `(val, record_id)` のタプル比較で継続位置へ直接飛べる。
駆動索引の並びがそのまま解の並びなので、**何ページ目でも同じコスト**。

**複数ソートキーの場合は事情が違う（正直に書く）。** pivot 索引はフィールドごとに別行なので、
`(field_a, field_b, record_id)` の複合順序を直接持つ btree は存在しない。第 1 キーが同値の
レコード群を、第 2 キー以降で並べ替える処理がどこかに要る。設計は次の 2 段構え。

1. **既定（同値群が小さい場合）**: 第 1 キーの索引で走査し、**同値群をページサイズの K 倍（既定 8 倍）まで
   先読みして、残キーでページ内ソート**する。同値群がこの範囲に収まる限り keyset は決定的に進む。
2. **同値群が上限を超える場合**: 第 1 キーの選択性が低すぎるので、**複合索引を材料化する**
   （`data_index_composite(tenant_int, table_int, sort_set_id, val_concat, record_id)` を
   §10 の自動昇格と同じ機構でオンデマンド生成する）。生成までは「このソート組み合わせは
   深いページで劣化する」ことを API が明示して返す。

**受け入れ条件には最悪ケース（第 1 キーが 1 値に偏ったテーブルでの複数ソート・深いページ）を含める**（§14）。

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

```text
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
    pub ref_table: TableId,
    pub cardinality: LinkCardinality,
    /// 参照先に自動生成する対称列（逆方向行の field_id）。None なら片方向リンク。
    pub symmetric_field: Option<FieldId>,
}

pub enum LinkCardinality {
    OneToOne,    // src 側・dst 側とも 1 件まで
    OneToMany,   // src 1 件 : dst 複数
    ManyToMany,  // 双方複数
}
```

- 値の表現は**常に配列**（`OneToOne` / `OneToMany` の片側は要素数 1 に制限）。
  v1 の単一 UUID 文字列からの移行は M1 で行う。
- **一意性は宣言だけでなく DB 制約で強制する**（`data_unique` はスカラー値用でリンク先をキーにしないため
  流用できない）。§3.3 の部分一意インデックス `(tenant_int, table_int, field_id, src_record) WHERE single_valued`
  1 本で両側を賄う。**2 行方式なので「dst 側の一意」は逆方向行に対する同じ制約になる**。

  | カーディナリティ | 順方向行の `single_valued` | 逆方向行の `single_valued` | 意味 |
  |---|---|---|---|
  | `OneToOne` | `true` | `true` | 双方 1 件まで |
  | `OneToMany`（src=one 側） | `false` | `true` | src は複数持てるが、各 dst は 1 つの src からのみ指される |
  | `ManyToMany` | `false` | `false` | 制約なし（`ord` の要素数上限のみ） |

  片方向リンク（`symmetric_field = None`）で dst 側一意が要る場合は、逆方向行を
  「制約専用行」として書く（`is_reverse = true` かつクエリからは参照しない）。
- **`single_valued` はレコード書込側が決めてはならない。** 部分一意インデックスの述語にフラグを使う以上、
  書込時に `false` を渡せば制約を迂回できてしまう。`data_link_field`（スキーマ改訂時にのみ書き換わる
  宣言メタデータ）への**複合外部キーで宣言値に固定する**（§3.3）。宣言が `true` のフィールドに
  `single_valued = false` の行を挿そうとすると参照先の組が存在せず FK 違反になる。
  カーディナリティを変更できるのは**スキーマ改訂経路のみ**で、そのとき既存行の再検証を行う。
- **対称フィールド**があれば、逆方向行を**同一トランザクションで同時に書く**（§3.3 の 2 行方式）。
  順方向だけを更新して逆方向が古いままになる状態は、同一 Tx なので発生しない。
- 参照整合性は書込時にサーバが検証（v1 の `validate.rs` を継承）
- `ord` の要素数には上限を設ける（PIT-48）

### 7.2 ルックアップ

参照先の列の値を引いて表示する。

```text
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
- `PerViewer` / `Volatile` なフィールドへの `indexed` / `unique` 宣言は**拒否**（422）
- ロールアップのスモールセル抑制は参照先の `aggregate_min_rows` を継承
- 詳細な脅威モデルは **PIT-47**

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

### 9.3 配信

```text
GET /data/tables/{table_id}/stream?since=<delivery_seq>     (SSE)
  event: record.upserted   id=<delivery_seq>   data={record_id, rev, fields...}
  event: record.removed    id=<delivery_seq>   data={record_id}   ← フィールドを含まない
  event: schema.changed    id=<delivery_seq>
  event: stream.reset      id=<delivery_seq>   ← 保持期間外。全件再取得を促す
```

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

```text
ユーザーが「備考」列でフィルタしようとする
  → 未索引 → 403 ではなく「この列での絞り込みを有効にしますか？」を提示
  → 小さいテーブル（閾値以下）は即座にバックフィルして透過的に有効化
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

> ⚠️ **行の post-filter だけでは足りない（フィールドマスクの穴）**。チャンクは書込時に静的生成されるため、
> 投影テンプレートにマスク対象列を含めると、**行自体が見えるユーザーは post-filter を通過し、
> マスクされているはずの列の内容まで回答・引用に流れる**。
> 例: 全社員に見える人事レコードの給与列だけを一般社員から隠していても、RAG 経由で給与が出る。
> **v1 の対処（fail-closed・単純）**: 投影テンプレートに**フィールドマスク対象列を含められない**よう
> スキーマ検証で拒否し、対象テーブルに `field_policy` が後から付いたら索引を破棄して再構築する。
> 「候補取得後に閲覧者本人のフィールドポリシーで再投影する」方式は、チャンク本文を検索インデックスに
> 置けなくなる（本文とベクタの生成を閲覧者ごとに分ける必要がある）ため、別設計として §15 に送る。

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
  **pivot 方式は駆動索引の選択を誤ると 100 倍遅くなる**ため、これは受け入れ条件に含める（→ **PIT-45**）。
  最低限カバーする最悪ケース:
  ①選択的フィルタ × 非選択的ソート／その逆 ②多条件 AND ③OR 木 ④keyset 継続（深いページ）
  ⑤**任意列（null あり）でのソート**——可視行が欠落しないこと
  ⑥**第 1 キーが 1 値に偏ったテーブルでの複数ソート**——同値群の先読み上限と複合索引の発動（§5.3）
  ⑦両方向のリンク走査がそれぞれ 1 パーティションに枝刈りされること
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
| **専用ストア（cell）／SQLite per tenant** | §12 第 3 段・判断記録は **§15.1**。強い隔離が要件化した顧客向け。**そのために物理層を Postgres 固有の飛び道具に依存させない** |
| **テナント引越ツール・実シャーディング** | §12 第 2 段。継ぎ目のみ先行 |
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

| Postgres | SQLite | 影響 |
|---|---|---|
| `numeric` | スケール済み int64 | 書込パス・集計の変換（上記のとおり解ける） |
| `= ANY($1::uuid[])` | 配列型なし → `json_each` か一時表 | **述語コンパイラの書き直し**（共有 id・ロール集合が全部これ） |
| `PARTITION BY HASH` | なし | テナント別 DB なら**そもそも不要**（利点） |
| `gin_trgm_ops` | FTS5 | 部分一致の実装が別物 |
| `FOR UPDATE` 行ロック | DB 全体の書込ロック | 並行性モデルが変わる |
| outbox・監査を同一 Tx | 別 DB | **リレー必須。監査ハッシュ連鎖は作り直し** |

コンパイラのバックエンド 1 本分の移植であり、設計をやり直す規模ではない。

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
4. **`PoolRouter` の継ぎ目**（§12 第 2 段・Phase 13.1）が、プール分割と専用ストアの**両方の入口**になる

#### 方針を変えるべき条件（これが起きたら再検討する）

| 条件 | 向かう先 |
|---|---|
| 事業が **PLG / セルフサーブで数千の小規模テナント**へ向かった | **SQLite per tenant を再検討**。この形なら所在管理層への投資が正当化される。**順序が重要で、後から数千テナントを Postgres プールから引き剥がす方が遥かに高くつく**ため、方針転換は早い段階で判断する |
| 特定顧客が**物理隔離を契約要件**にした | **専用 Postgres（cell）**。SQLite ではない（運用の既知性を取る） |
| 総量 10TB 級 or クジラ顧客 | **プール分割**（`PoolRouter` の実分割・§12 第 2 段） |

---

## 16. 性能設計

> §1.2 の目標（グリッド初回描画 p95 < 300ms・1 ページ p95 < 100ms）を満たすための設計。
> **駆動索引の選択**（PIT-45）は §3.5 と §14 に既出なので、ここでは残りを扱う。

### 16.1 SQL 互換インターフェース —— 前段言語として置く

#### なぜ必要か

理由は 3 つあるが、本プロダクトで最も効くのは **LLM の出力精度**である。

「経費テーブルの承認待ちを金額順に」に対して、LLM は SQL なら高い確度で正しく書く。
一方 `DataQuery` の JSON IR は世界に存在しない記法なので、ツールの description に文法を書いても
生成が安定せず、検証エラー → 自己修正のラウンドが伸びる。
**チャットからテーブルを操作する体験（§11・Phase 13.7）の質はここでほぼ決まる。**
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
| `MAX_COMPILE_MS` | 50 ms | ①〜⑥のコンパイル全体の時間予算 |

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
- 自己結合・多段結合の深さに上限を置く（`MAX_JOIN_DEPTH`）

リンク列経由なら `data_link` の 2 行方式でパーティション枝刈りも効く。
**JOIN の自由度を制限する理由は性能ではなく authz である。**

#### エラー契約 —— 列の存在オラクルを作らない

拒否理由は構造化して返す（`EmitUiTool` が検証エラー全件を返してモデルに自己修正させる前例と同じ流儀）。
**ただし「存在しない列」と「存在するが見えない列（フィールドマスク対象）」は同一のメッセージ・
同一のエラーコードで拒否する。** 区別すると、SQL 面が**列の存在オラクル**になり、
マスクした列名の有無を総当たりで確認できてしまう。テーブル名についても同様
（`data_table` の viewer でないテーブルは「存在しない」と同じ応答にする）。

### 16.2 最適化層（AST があるから可能なもの）

| 最適化 | 内容 |
|---|---|
| **駆動索引選択** | フィールド単位統計から選ぶ（PIT-45・§14） |
| **述語の並べ替え** | 選択率の高い `EXISTS` を先に評価させる |
| **射影刈り込み** | `SELECT` に無い列を本体から取らない |
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

> **Phase 13.2 のバックフィル自体がこの経路を要求する**ため、移行計画の前提として先に要る。

### 16.8 共有プールの公平性

フルプールなので、1 テナントの重いクエリが他テナントを圧迫する。
`statement_timeout` は**上限であって公平性ではない**。

→ **テナント単位の同時実行数制限**を読取経路へ入れる
（workflow-engine のトークンバケット実装を再利用・design §4.12 の API レート制限と同じ枠）。

### 16.9 残る未設計項目

| 項目 | 内容 |
|---|---|
| 集計の事前計算 | ダッシュボードの集計が毎回全走査。`Invariant` なテーブルに限り事前集計してよい（§7.4 と同じ規則が流用できる） |
| 件数クエリ | 上限付きでも 2 本目のクエリ。非同期化するか要求時のみに絞るか |
| lookup の N+1 | フィールドごとに 1 クエリ。5 リンク列のグリッドで +5 本。1 本にまとめられる |

### 16.10 優先順位

1. **§16.4（FGA 往復のメモ化）** — 取るのが安く、効果が最大
2. **§16.1 + §16.2（SQL 互換面と最適化層）** — チャット体験の質を決める。パーサは依存済み
3. **§16.3（形状固定とプランキャッシュ）** — 効果は中だが、後から入れにくいので最初に
4. **§16.6（スキーマキャッシュ）** — 全クエリへ効く
5. **§16.7（一括書込）** — Phase 13.2 のバックフィルが要求するので前倒し
6. **§16.5（covering read）** — 条件付き。統計基盤ができてから
