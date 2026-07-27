# Phase 13 — 構造化データ基盤 v2（Teable / Microsoft Lists 級）

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

**13.1〜13.3（再基盤化）はプライベートアルファ前の必須項目**である。理由は 2 つ。

1. **現行の索引方式はフルプールで破綻する。** v1 は索引宣言フィールドごとに `data_record` 上へ
   partial 式インデックスを張るため、索引本数が**テナント数 × テーブル数 × 索引列数**で増え、
   それが単一 relation にぶら下がる。PostgreSQL は 1 行の INSERT で全索引を open し partial 述語を
   全件評価するため、**顧客 20〜50 社で劣化し数百社で停止する**（data-platform.md §1.1）。
   SaaS のデータプレーンはフルプール（design §4.1・SAAS.5 達成済み）なので、これはリリースブロッカー。
2. **データが小さい今が移行の最安時機。** 13.1（JSONB キー書換）と 13.2（索引バックフィル）は
   レコード数に比例する。顧客が入ってからでは移行コストが跳ね上がる。

13.4 以降（機能拡張・UI・各面公開）はアルファと並走してよい。

## 依存

- **Phase 9**（`crates/data` v1・4 階層 authz・ゲートウェイ能力面）
- **Phase 10**（workflow-engine の IR `Condition` 語彙・ノードカタログ・`effect_journal`）
- **Phase 6**（generative UI の `ActionBinding` / `ActionDispatcher`）
- **Phase 11-pre**（グリッド UI の前例 `web/src/components/csv/csv-grid.tsx`・glide-data-grid）

> ⚠️ **着手前に [設計上の落とし穴](../design-caveats.md) の PIT-45〜49 を確認すること。**
> v1 の PIT-17〜21（集計からの特定・述語材料の量・マスク列での並べ替え・テーブル間の権限素通り・
> WHERE 強制注入の限界）は**そのまま生き続ける**。v2 はこれに加えて、
> pivot 索引のプランナ統計欠如と駆動索引の不在（PIT-45）・SSE の権限材料鮮度と述語グループ化の破綻（PIT-46）・
> 計算列の材料化残留（PIT-47）・書込増幅と肥大化とバックフィル競合（PIT-48）・
> 可視性を失った行の配信漏れ（PIT-49）を持ち込む。

| ID | タイトル | area | 依存 |
|----|---------|------|------|
| 13.0 | フルプール正本化（ドキュメント整合）＋ data-platform.md 制定 | infra | – |
| 13.1 | フィールド識別子 3 層移行＋内部整数キーレジストリ＋PoolRouter 継ぎ目 | data | 13.0 |
| 13.2 | pivot 索引テーブル導入＋二重書込＋オンラインバックフィル | data | 13.1 |
| 13.3 | クエリコンパイラ v2（条件木・keyset・統計・EXPLAIN ゲート）＋読取切替＋旧方式撤去 | data | 13.2 |
| 13.4 | リンク／ルックアップ／ロールアップ／数式＋Materialization 規則 | data | 13.3 |
| 13.5 | 型付き ViewSpec ＋ フロント（テーブル一覧・グリッド・スキーマエディタ） | frontend | 13.3 |
| 13.6 | レコード変更 outbox ＋ SSE 差分配信 ＋ 索引の自動昇格 | data | 13.3 |
| 13.7 | 各面公開: workflow data ノード／shiki script／generative UI 束縛／chat ツール | api | 13.3, 13.4 |
| 13.8 | RAG 統合スパイク（構造化データの permission-aware 検索） | rag | 13.3 |

---

## 詳細

### Task 13.0: フルプール正本化（ドキュメント整合）＋ data-platform.md 制定
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
  - [ ] 「cell 隔離が正」と読める記述が正本ドキュメントに残っていない（将来オプションとしての言及は可）
  - [ ] design §4.1 ⇔ §4.1.1 ⇔ requirements §1.1 ⇔ roadmap の SaaS トポロジ記述が矛盾しない
  - [ ] テナント消去（design §4.12・Phase 12.9）が「ストアごと捨てられない」前提で書き直されている

### Task 13.1: フィールド識別子 3 層移行＋内部整数キーレジストリ＋PoolRouter 継ぎ目
- **area**: data / **path**: `crates/data/src/model.rs`, `crates/data/src/schema.rs`, `crates/storage/src/tenant.rs`, migrations
- **依存**: 13.0
- **仕様**:
  - `FieldDef` を **`id`（u16・不変・JSONB キー `f{id}`・索引の `field_id`。`0` は owner 擬似列に予約）/
    `key`（`^[a-z][a-z0-9_]{0,63}$`・不変・script/IR/SDK の参照名）/ `display_name`（自由文・**日本語可**・可変）**
    の 3 層に分離する。`order` / `description` / `searchable` も追加（data-platform.md §4）。
  - 内部整数キーを導入: `tenant_int int4`（tenant レジストリで採番）・`table_int int8`（グローバル連番・
    後のパーティションキー）。**採番は不変・再利用しない**。
  - 既存データの移行: `name → key`、`display_name = name`、`id` は宣言順に 1 から採番、
    JSONB のキーを `f{id}` へ書き換える（テーブル単位・jobq バッチ・中断再開可）。
  - **`tenant → pool` 解決の継ぎ目を単一チョークポイントに入れる**: `DataStore` が `PgPool` を直接持つのをやめ、
    `PoolRouter` から取る形にする。**継ぎ目を後から入れるのは高いので先に入れる**（実分割はしない・
    data-platform.md §12 第 2 段）。あわせて**シャード安全の禁止事項**（テナント横断 JOIN 禁止・
    グローバル連番への依存禁止・横断集計は合算で書く）を文書化し、可能なものは CI で検査する。
- **受け入れ条件**:
  - [ ] 日本語の列名（`display_name`）でテーブルを作成・表示・変更でき、変更時に `data_record` が 1 行も更新されない
  - [ ] `key` の変更が API で拒否される（不変性の強制）
  - [ ] 既存テーブルの移行がテーブル単位で冪等に実行でき、中断後に再開できる
  - [ ] 移行後も v1 の IT（`data_it.rs` / `policy_threat_it.rs` / `query_view_it.rs` / `fsm_it.rs`）が全て通る
  - [ ] `crates/data` 内から `PgPool` を直接参照している箇所が無い（PoolRouter 経由に統一）

### Task 13.2: pivot 索引テーブル導入＋二重書込＋オンラインバックフィル
- **area**: data / **path**: `crates/data/src/index/`, migrations, `crates/jobq`
- **依存**: 13.1
- **仕様**:
  - 索引テーブル群を追加（data-platform.md §3.3。すべて `PARTITION BY HASH (table_int)` 64 分割）:
    `data_index_text`（text/select/multi_select/date/datetime/*_ref/status＋システム擬似列）/
    `data_index_num`（number・`numeric`）/ `data_link`（record_ref）/
    `data_unique`（unique 制約を PK 1 本で賄う）/ `data_index_search`（部分一致・trigram GIN の隔離先）。
  - **索引の総本数はパーティションあたり 10 本の定数**とし、テナント数・テーブル数・列数に依存させない。
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
    **代償として漢字の並び順は符号位置順**になるため、読み仮名列でのソート指定を 13.5 の ViewSpec で受ける。
  - 多値列（`multi_select` / `link`）の 1 レコードあたり要素数上限を設ける（PIT-48）。
- **受け入れ条件**:
  - [ ] 索引テーブルの本数がテーブル数・テナント数に依存しないことを、テーブルを大量生成する IT で確認する
  - [ ] バックフィルが中断・再開でき、完了後に旧索引と新索引の内容が一致する（parity 検証ジョブ）
  - [ ] 書込パスで変更のないフィールドの索引行が更新されない（差分更新の確認）
  - [ ] 多値要素数の上限超過が 422 で拒否される
  - [ ] `data_record` のパーティション枝刈りが効く（`EXPLAIN` で 1 パーティションのみ走査）
  - [ ] **リンクの順引き・逆引きがいずれも 1 パーティションに枝刈りされる**（`EXPLAIN`）
  - [ ] **`MAX_TEXT_LEN` 上限（10,000B）の値を持つレコードが `indexed` 列でも書き込める**
        （btree キー超過で落ちない）／切り詰め値の等値検索が本体再確認で正しく効く
  - [ ] 部分一致検索が他テナントのデータ量に影響されない（GIN スコープの確認）
  - [ ] **拡張が無い環境で初回マイグレーションが明示的に失敗する**（`pg_trgm` / `btree_gin` の事前確認）
  - [ ] **`OneToOne` / `OneToMany` の一意性違反が DB 制約で弾かれる**（アプリ検証を迂回しても書けない）
  - [ ] **`single_valued = false` を明示的に渡しても、宣言が `true` なら FK 違反で書けない**
        （フラグ経由の制約迂回ができないことの確認）
  - [ ] `FieldId` が 32,767 を超える採番要求が 422 で拒否される
  - [ ] **切り詰めが起きる列でのソートが keyset で重複・欠落しない**
        （`(val_prefix, record_id)` のタプル比較が決定的であることを、同一プレフィクス多数のデータで確認）

### Task 13.3: クエリコンパイラ v2 ＋読取切替＋旧方式撤去
- **area**: data / **path**: `crates/data/src/query/`, `crates/data/src/policy/compile.rs`
- **依存**: 13.2
- **仕様**:
  - **クエリ IR を刷新**（data-platform.md §5）: `Condition` の AND/OR/NOT 条件木・型別演算子・
    複数ソートキー・**keyset カーソル（OFFSET 廃止）**・投影（`select`）・`group_by` ＋ 複数 `aggregate`・
    横断部分一致（`search`）。**演算子語彙は workflow IR の `Condition` に揃える**
    （`data.query` ノードで変換層が不要になる）。`matches`（正規表現）は索引が効かず DoS 面のため**採用しない**。
  - **行述語のコンパイル先を索引テーブル条件に変える**。`PolicyExpr` の AST・材料解決（`material.rs`）・
    fail-closed の閾値はそのまま。`IsOwner` は owner 擬似列（`field_id=0`）で評価する。
    **`row_policy` が参照するフィールドは `indexed` 必須**とし、スキーマ検証で強制する（data-platform.md §6.3）。
  - **駆動索引の選択をコンパイラが行う**。フィールド単位の粗いカーディナリティ統計をレジストリに持ち、
    背景ジョブで更新する。プランナ任せにしない（PIT-45）。
  - 索引到達可能性の強制（§5.2）: filter/sort/group_by/aggregate は `indexed || unique` 宣言済みのみ、
    マスク列は 403、`Contains`/`EndsWith` は `searchable` 宣言済みのみ、`statement_timeout`。
    条件は「**駆動索引が 1 つ以上決まること**」とし、「条件木に索引可能な述語を最低 1 つ」という
    強い形にはしない（`filter = None` の初期グリッド＝13.5 の要件が常に拒否されるため）。
    システム擬似列は常に駆動になれるので初期グリッドは成立する。
  - **複数ソートの最悪ケースを設計に含める**。pivot 索引はフィールドごとに別行なので複合順序の btree が
    存在せず、第 1 キー同値群が大きいと第 2 キー以降の再ソートが要る。既定は同値群をページサイズの
    K 倍（既定 8）まで先読みしてページ内ソート、超過時は複合索引をオンデマンド材料化する。
  - 件数は**上限付き正確カウント**（`Exact(n)` / `AtLeast(10_000)`）。上限はテナント設定で変更可能。
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
  - [ ] 条件木（AND/OR/NOT）・複数ソート・keyset ページングが動き、深いページでも応答時間が一定
  - [ ] 100 万行のテーブルでグリッド 1 ページ（100 行）が p95 < 100ms
  - [ ] `policy_threat_it.rs` が新旧両実装で通る／parity 検証が全代表クエリで一致
  - [ ] **EXPLAIN 回帰テストが CI にある**（選択的フィルタ×非選択的ソート／その逆／多条件 AND／OR 木／
        keyset 継続／**任意列ソート**／**第 1 キーが偏った複数ソート**の各パターンで駆動索引と実測行数を
        アサート・PIT-45）
  - [ ] **任意列（null あり）でソートしても可視行が欠落しない**（未設定レコードが結果から消えない）
  - [ ] `filter = None` の初期グリッド（フィルタなし 100 行）が拒否されず p95 < 100ms で返る
  - [ ] `row_policy` が未索引フィールドを参照するスキーマが 422 で拒否される
  - [ ] マスク列を filter/sort/group_by/aggregate/search に指定すると 403（PIT-19 継承）
  - [ ] 集計のスモールセル抑制と集計クエリ監査が v1 と同じ挙動（PIT-17 継承）
  - [ ] `grep -r "CREATE INDEX" crates/data/src` が 0 件（ランタイム DDL ゼロ）

### Task 13.4: リンク／ルックアップ／ロールアップ／数式＋Materialization 規則
- **area**: data / **path**: `crates/data/src/derived/`, `crates/data/src/formula/`
- **依存**: 13.3
- **仕様**:
  - **双方向リンク**を `data_link` の順引き PK と逆引き索引で実装する（1:1 / 1:N / N:N を `ord` の多値で表現）。
    対称フィールド（参照先に現れる逆リンク列）は**スキーマ上のメタデータ**とし、
    実体は同じ行を逆から読む（**二重書込をしない＝不整合が構造的に発生しない**）。
  - **ロールアップ**（count/sum/avg/min/max/concat）と**数式**を追加。数式は**閉じた AST ＋ Rust インタプリタ**
    （自由文字列を SQL にしない）。算術は `numeric`。**依存 DAG をスキーマ保存時に構築し循環を 422 で拒否**。
  - **`Materialization { Invariant, PerViewer, Volatile }` を導入する**（data-platform.md §7.4）:
    `Invariant`（全閲覧者・全時刻で同値と証明できる）だけが書込時材料化・索引可。
    `PerViewer` / `Volatile` は**読取時計算のみ・キャッシュ禁止・索引/ソート/フィルタ不可**。
  - **`Invariant` の判定には 4 階層すべてを見る**（1 つでも閲覧者依存なら `PerViewer`）:
    ①参照先テーブルの **ReBAC viewer 集合が参照元を包含すると証明できるか**（できないのが既定）
    ②参照先の `row_policy` ③参照先の `field_policy` ④参照先の**レコード個別共有タプルの存在**。
    多段参照は推移的に辿る。`Now` を含む式は `Volatile`。
  - **降格トリガは 4 階層すべての変化**（policy 追加・ReBAC 変更・個別共有の発生）。降格時は
    材料化データと索引行を**同一トランザクションで破棄**する。これを忘れると漏洩が残留する（PIT-47）。
  - **参照先レコードの変更でも材料化値は陳腐化する**（金額更新・行の作成/削除・リンク付け替え）。
    依存 DAG を**実データの逆依存追跡**にも使い、参照先の変更 Tx から影響する参照元を特定して
    同一 Tx で再計算するか世代付きジョブへ enqueue し、**再計算完了まで当該列をクエリ対象外にする**。
  - **リンクのカーディナリティをスキーマで宣言する**（`LinkDef { ref_table, cardinality, symmetric_field }`）。
    値表現は常に配列。`OneToOne` / `OneToMany` の一意性は `data_unique` 相当で強制する
    （v1 の `record_ref` は単一 UUID 文字列しか受け付けず N:N を作れなかった）。
  - lookup / rollup / 数式のすべてで**参照先テーブルの行ポリシーを閲覧者本人の権限で透過適用**する
    （PIT-20 の全面解決）。ロールアップのスモールセル抑制は参照先の `aggregate_min_rows` を継承。
- **受け入れ条件**:
  - [ ] N:N リンクの両方向が引け、**両方向とも 1 パーティションに枝刈りされる**（EXPLAIN で確認）
  - [ ] `OneToOne` / `OneToMany` の一意性違反が 422 で拒否される
  - [ ] `PerViewer` / `Volatile` 列への `indexed`/`unique` 宣言が 422 で拒否される
  - [ ] **参照先テーブルの viewer 権限を持たないユーザーに、参照先の全件から計算したロールアップが
        見えない**（テーブル ReBAC 迂回の negative IT・PIT-47）
  - [ ] **材料化済みテーブルの参照先に `row_policy` を後付け／個別共有を 1 件作成／ReBAC を変更**した
        いずれの場合も、材料化データが破棄され値が読めなくなる（`policy_threat_it.rs` に追加・PIT-47）
  - [ ] 参照先レコードの金額を更新すると、参照元の材料化ロールアップが再計算される
        （再計算完了までは当該列がクエリ対象外になる）
  - [ ] 見えない参照先の lookup/rollup 値が null / 抑制される（PIT-20）
  - [ ] 数式の循環参照が 422 で拒否される
  - [ ] 金額の四則演算・集計が `numeric` で正確（浮動小数点誤差が出ない）

### Task 13.5: 型付き ViewSpec ＋ フロント
- **area**: frontend / **path**: `crates/data/src/view.rs`, `web/src/app/(auth)/data/`, `web/src/components/data/`
- **依存**: 13.3
- **仕様**:
  - `DataViewBody.display`（サーバ非解釈の JSON）を **型付き `ViewSpec`** へ置き換える。
    `#[serde(tag = "kind")]` のタグ付き enum: `Grid` / `Kanban` / `Calendar` / `Gallery`
    （`Form` は v1 スコープ外・data-platform.md §15）。**Rust 型を単一ソースとし ts-rs で TS 生成**。
  - `ColumnConfig`（表示順・幅・固定・非表示・**ソート代替列**＝日本語の読み仮名列）・行グループ化・
    条件付き書式（閉じた条件木）。**サーバが検証**する（存在しない `field_id`・マスク列の表示指定・
    `PerViewer` 列でのソート指定を拒否）。
  - 保存は v1 どおり `artifact(kind=data_view)` の枠（ReBAC 共有・不変バージョン）。
    **実行は必ず `run_query` 経由**で閲覧者本人の権限で毎回再評価（作成者の権限を引き継がない）。
  - フロント: `/data` ルート（`web/src/lib/nav-config.ts` に追加）・テーブル一覧・**仮想化グリッド**
    （`csv-grid.tsx` の glide-data-grid＋ページキャッシュの前例を踏襲）・セル編集（`rev` 楽観ロック・409 は再読込）・
    スキーマエディタ（列の追加/並べ替え/表示名変更/索引トグル）・ビュー切替・フィルタ/ソート UI。
    API クライアントは `web/src/lib/data-api.ts`（型は `@/generated/api` から・手書き型を作らない）。
- **受け入れ条件**:
  - [ ] グリッドで 100 万行のテーブルを無限スクロールでき、初回描画 p95 < 300ms
  - [ ] 列の表示名変更・並べ替え・幅変更・非表示がビューに保存され、他ユーザーに影響しない
  - [ ] マスク列や `PerViewer` 列を不正に指定した ViewSpec が保存時に拒否される
  - [ ] 保存ビューを別ユーザーが開くと、そのユーザーの権限で行が絞られる
  - [ ] E2E（`web/e2e/data-grid.spec.ts`）でテーブル作成→列追加→レコード編集→ビュー保存が通る

### Task 13.6: レコード変更 outbox ＋ SSE 差分配信 ＋ 索引の自動昇格
- **area**: data / **path**: `crates/data/src/record.rs`, `crates/api/src/routes/data_stream.rs`
- **依存**: 13.3
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
  - **採番だけでは足りず、購読者ごとの送信順序も契約にする**。`claim_undelivered` は順序非依存なので
    リレーが `delivery_seq=10` を先に送り `9` を後に送り得る。10 で切断されると `since=10` の再開で
    **9 が永久に欠落する**。①リレーは 1 購読者への送信を `delivery_seq` 昇順に揃える
    ②再開カーソルは**連続確認済みの low-water mark** だけを進め、飛び番では進めない
    ③クライアントへ返す `Last-Event-ID` はこの low-water mark であり受信済み最大値ではない。
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
    検知した購読は TTL を待たず即時無効化する（PIT-46）。
  - **索引の自動昇格**: 未索引列でのフィルタ/ソート要求を 403 で終わらせず、
    小さいテーブルは即座にバックフィルして透過的に有効化、大きいテーブルは jobq で
    進捗表示つきバックフィル。テーブルあたりの索引列数に上限を設け、超過時は明示承認を要求する（PIT-48）。
  - **索引状態を `absent` / `building` / `active` の 3 値にし、`building` 中は write-through する**。
    「進行中はクエリに使わない」だけでは競合を防げない——走査済み領域のレコードが有効化前に更新されると、
    その変更は**完成後の索引から永久に欠落する**。`building` に遷移した時点から新規書込を索引へ二重反映し、
    スナップショット走査と差分が収束してから原子的に `active` へ切り替える。
  - CRDT（Yjs）は**採らない**。行・列単位で可視性が異なり authz モデルと衝突するため
    （`crates/collab` はノート/スライド用途に留める）。編集はサーバ権威＋`rev` 楽観ロック。
    他ユーザーのカーソル/選択範囲は awareness チャネル（データを含まない）で配信。
- **受け入れ条件**:
  - [ ] 他ユーザーの編集が 1 秒以内にグリッドへ反映される
  - [ ] **見えない行・マスク列が SSE に流れない**（negative IT）
  - [ ] **可視→不可視に変わった購読者へ `record.removed` が届き、以後その行が表示されない**（negative IT）
  - [ ] 再接続時に `Last-Event-ID` から欠落・重複なく再開できる（並行コミットを含むシナリオ）／
        保持期間外は `stream.reset` が返る
  - [ ] **飛び番を受信した直後に切断しても、間の `delivery_seq` が再開後に配信される**
        （low-water mark 方式の確認・順序非依存リレーとの組み合わせで欠落しない）
  - [ ] 権限剥奪後 TTL 以内に配信が止まる／TTL を超えて配信が続かない／REST 経路は TTL の影響を受けない（PIT-46）
  - [ ] **`IsOwner` ＋ 個別共有を含む行ポリシーで、購読者 1,000 人・ロール 3 種のとき述語評価が 3 回**（計測）
  - [ ] バックフィル進行中の列がクエリ対象にならない／**`building` 中の書込が完成後の索引に反映される**

### Task 13.7: 各面公開（workflow / shiki script / generative UI / chat）
- **area**: api / **path**: `crates/workflow-engine`, `crates/script-runtime`, `crates/gui`, `crates/chat`
- **依存**: 13.3, 13.4
- **仕様**:
  - **ワークフロー data ノードの解禁**: `NodeType::{DataQuery, DataRecordCreate, DataRecordUpdate,
    DataRecordDelete, DataTransition, DataBulkUpsert}` を `available_stage_a` へ入れ、
    `Scope::{DataRead, DataWrite}` を保存可能スコープに追加（現状 V3 が `ir.unknown_scope` で拒否）。
    `ir/params/data.rs` を新設し、`nodes/exec.rs` の dispatch・`NodePorts`・`ProdNodePorts` を配線。
    **IR `Condition` と `DataQuery.filter` が同語彙なので変換層を作らない**。
    書込は `effect_journal`（`IdempotencyClass::EngineDedup`）を通す。
    ノードカタログへ登録すれば `EmitWorkflowTool` の description に自動反映される。
  - **shiki script（ワークフロー/skill 経路）**: `HostBridge::dispatch`（`nodes/script.rs`）に
    `data.*` アームを追加する。api 名は既に `ALLOWED_APIS` にあるため追加不要。
    ゲスト側は `MINIAPP_PRELUDE` 方式（wasm 再ビルド不要）を踏襲する。`data.transition` も公開する。
  - **generative UI**: `ActionBinding` に第 4 種 `DataView { view_id, pinned_version }` を追加し、
    `data_grid` コンポーネントをデータ束縛で描画する。**クライアントが送れるのは `action_id + params` のみ**
    という `ActionDispatcher` の不変条件を維持し、束縛定義はサーバが照合する。
    `SpecValidator` が保存時に閲覧権限を解決してバージョンをピンする（`Workflow` 束縛と同じ流儀）。
  - **chat / agent-core**: `data_query`（読取）・`data_record_write`（`requires_confirmation = true`）・
    `data_schema`（グラウンディング用）を追加する。`csv_tool.rs` の 3 ツールが最も近い先例。
    description には**発話ユーザーが viewer のテーブル一覧を動的に載せる**（`SkillTool` と同じ流儀・
    テーブル数は有界）。`worker/toolset.rs` の「未配線なら提示しない」ポリシに従う。
- **受け入れ条件**:
  - [ ] スケジュールトリガのワークフローが `data.query` → `data.record.update` を実行でき、
        **委譲元ユーザーの権限**で行が絞られる
  - [ ] shiki script から `Shiki.data.query` が呼べ、スコープ ceiling 外の呼び出しが `out_of_scope` で監査される
  - [ ] チャットで「経費テーブルの承認待ちを金額順に見せて」が動き、見えない行が返らない
  - [ ] generative UI のテーブルがデータ束縛で描画され、**閲覧者本人の権限**で毎回再評価される
  - [ ] 全面で同一の `DataQuery` 型が使われている（面ごとの独自フィルタ表現が存在しない）

### Task 13.8: RAG 統合スパイク（構造化データの permission-aware 検索）
- **area**: rag / **path**: `crates/rag`（設計スパイク）
- **依存**: 13.3
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
  - 増分索引は 13.6 の outbox に相乗り。`SearchResult` の多型化（file 前提を崩す）が必要。
  - **本タスクは設計スパイクまで**。実装はポストアルファ可。
- **受け入れ条件**:
  - [ ] `SearchResult` 多型化と post-filter 一般化の設計が docs に落ちている
  - [ ] 行レベル権限が検索結果に効くことを検証する PoC がある

---

## 参照

- 設計正本: [data-platform.md](../data-platform.md)
- 落とし穴: [design-caveats.md](../design-caveats.md) PIT-17〜21（v1 から継承）・**PIT-45〜49**（v2 が持ち込む）
- v1 の実装: [phase-9.md](./phase-9.md) Task 9.2〜9.5・9.10
- ワークフロー連携: [phase-10.md](./phase-10.md)（Stage B の data 系ノード）
- テナンシー前提: [design.md](../design.md) §4.1（フルプール）
