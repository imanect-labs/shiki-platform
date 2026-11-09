# ワークフロー IR 仕様

> 本書は [miniapp-platform.md](../miniapp-platform.md) §2.1 の詳細設計。概念・スコープの正本は [miniapp-platform.md](../miniapp-platform.md)。
> 認可・ストレージ・LLM の不変条件（単一チョークポイント・AuthContext・二重ゲート）は [design.md](../design.md) §1/§4 が正本であり、本書はその上に載る。
> 実装は [roadmap Phase 10](../roadmap/phase-10.md)（Task 10.1 が本書の主対象。10.5/10.6/10.10/10.12/10.13 が参照）。
> 着手前に [design-caveats](../design-caveats.md) の PIT-31（at-least-once 副作用）・PIT-34（委譲失効の検知）・PIT-35（script ホスト関数ブリッジ）・PIT-36（シークレット宛先束縛の迂回）を確認すること。
> 本書は IR の**定義と静的制約**の正本。実行時の振る舞い（skip 伝播の実行・join 発火・冪等・リトライ）は [engine.md](./engine.md)、script ノードのコンパイル・実行は [script.md](./script.md) が正本。用語は [README.md](./README.md) の用語集に一元化する。
> 本数値上限・タイムアウト・プール数はすべて**初期値**（実装時にベンチ・運用で調整。コード上は設定値）である。

---

## 1. 位置づけ

IR（Intermediate Representation）はワークフロー定義の**唯一の正本**であり、JSON で表現された有向非巡回グラフ（DAG）である。「トリガ＋ノード＋接続＋ポリシ」を宣言的に持つ。

- **IR を編集するのは dnd エディタと AI 編集の 2 つだけ**（[miniapp-platform.md](../miniapp-platform.md) §2.1）。shiki script は IR の編集手段では**ない**——ノードの中身を実装する言語であり、IR とは層が異なる（[script.md](./script.md) §1）。
- **code view（IR→script の双方向同期）はスコープ外**。IR とスクリプトを往復変換する経路は最初から作らない。IR は宣言的スペックのまま保つ。
- IR はバージョン付き artifact である（Phase 6 の artifact 共通枠を再利用: 不変 version・ReBAC 共有・監査）。artifact の `kind` は `workflow`。
- **保存時に検証パイプライン（§8）を通ったものだけが artifact version になる。検証を通らない IR は存在できない**。dnd 保存も AI 生成も同一の入口・同一のパイプラインを通る（generative UI の「検証済みスペック」パターンの再利用）。AI が生成した IR も人間が dnd でそのまま続きを編集できるのは、両者が同じ検証済みスペックを共有するからである。

IR が担う責務と、IR が担わない責務を明確に切り分ける。

| 担う（IR = 配線・定義） | 担わない（別の層） |
|---|---|
| ノードの配置・接続・パラメータの静的定義 | ノード内部のロジック（→ script ノード / [script.md](./script.md)） |
| トリガ・リトライ・タイムアウトの宣言 | 実行時の状態遷移・チェックポイント（→ [engine.md](./engine.md)） |
| 参照・条件・テンプレートの構造化記述 | 自由記述の式・計算（→ script ノードへ昇格・§3） |
| 保存時に静的検証可能な制約 | 実行時にしか分からない値の解決（→ [engine.md](./engine.md)） |

### 1.1 IR の構成要素関係図

```mermaid
flowchart TB
  ENV["エンベロープ（§2）<br/>ir_version / name / declared_scopes / policies"]
  TRG["triggers[]（§6）<br/>schedule / event / interactive"]
  NOD["nodes[]（§4）<br/>id / type / params / retry / timeout"]
  EDG["edges[]（§5）<br/>from / from_port / to"]
  ISC["input_schema（§2）<br/>対話トリガの入力フォーム契約"]
  REF["データフロー模型（§3）<br/>$from / $template / condition"]

  ENV --> TRG
  ENV --> NOD
  ENV --> EDG
  ENV --> ISC
  TRG -.->|ペイロードを参照| REF
  ISC -.->|入力を参照| REF
  NOD -.->|params 内で使う| REF
  NOD -->|ポートで接続| EDG
  EDG -->|待ち合わせは入エッジ集合| NOD
  NOD -->|control.map の parent| NOD
```

---

## 2. エンベロープ

IR のトップレベルは以下の形を取る。

```jsonc
{
  "ir_version": 1,                  // IR スキーマ版（整数・単調増加）。未知の版は保存拒否（§9）
  "name": "expense-approval",       // ^[a-z][a-z0-9-]{0,63}$。tenant 内一意は artifact 層で担保
  "display_name": "経費承認フロー",  // 表示用
  "description": "経費申請の金額に応じて承認経路を分岐する",
  "declared_scopes": ["data.read", "data.write", "notify.send"],  // 宣言スコープ＝権限の天井。codegen 認可語彙
  "triggers": [ /* §6 */ ],
  "input_schema": { /* JSON Schema サブセット・省略可 */ },  // 対話トリガの入力フォーム契約
  "nodes": [ /* §4 */ ],
  "edges": [ /* §5 */ ],
  "policies": {                     // 省略時は全て既定値
    "run_timeout_sec": 259200,      // run 全体の上限（既定 3 日・最大 30 日）
    "concurrency": { "max_parallel_runs": 10 },  // このワークフローの run 同時実行（テナント/ノード種上限は engine 側設定）
    "on_trigger_overflow": "queue"  // 上限超過時: queue（既定・バックプレッシャ）| skip
  }
}
```

- **deny-unknown（全階層）**: 未知フィールドは全ての階層で保存拒否する。セキュリティ製品として未知の入力を黙殺しない（§8 V1）。この方針の帰結として、フィールド追加であっても `ir_version` を上げる（§9）。
- **`name`**: `^[a-z][a-z0-9-]{0,63}$`。tenant 内一意性の担保は artifact 層の責務であり、IR 検証はフォーマットのみを見る。
- **`declared_scopes`**: codegen 認可語彙の閉じた集合に照合する（§8 V3）。これは**権限の天井**であり、実効権限は次式で定まる（[miniapp-platform.md](../miniapp-platform.md) §7 の表そのまま）。

  ```
  実効権限 = トリガ由来の主体権限（実行主体の ReBAC） ∩ declared_scopes ∩ ノード設定
  ```

  **新バージョンで `declared_scopes` が広がった場合、有効化済みワークフローは再同意が済むまで新バージョンへ切替不可**（fail-closed。実行時の強制は [engine.md](./engine.md) §10 と対応）。縮小のみなら差分同意なしで切替可能。
- **`policies`**: 全て省略可。省略時は既定値。`run_timeout_sec` は run 全体の壁時計上限、`max_parallel_runs` はこのワークフロー単位の run 同時実行上限、`on_trigger_overflow` は上限超過時の挙動（`queue` はバックプレッシャで滞留、`skip` は run 作成を見送る。実行時の滞留・滞留解除の詳細は [engine.md](./engine.md) §8）。`max_parallel_runs` が数えるのは実行中の run で、**wait（時間・できごと）で止まっている run は数えない**（承認やイベントを何日も待つ run が枠を占めて、新しい起動が進まなくなるのを防ぐ。起床して上限を一時的に超えるのは許す・D-26）。
- **`input_schema`**: 対話トリガ（§6 `interactive`）の入力フォーム契約。JSON Schema のサブセット（型・必須・enum・range 程度。式・`$ref` 外部参照は持たない）。`llm.invoke` の `output_schema` は同じキーワードの集合に、構造化出力のための追加の制約（根は object・オブジェクトは閉じる・上限など）を課したもの（§7.3）。`format` は `date`（`YYYY-MM-DD` の実在する日付）だけを起動の前に検査し、ほかの値は注記として読み飛ばす（画面の日付の欄が送る形を、API・会話の図からの起動でも守らせる・BUG-35）。
  - **項目の並び（`x-order`）**: 注記 `"x-order": ["title", "amount"]`（`properties` のキーの配列）が、画面に並べる項目の順を持つ。IR は jsonb の列に保存されるので、`properties` のキーの順は保存して読み直すと辞書順に変わる（BUG-34・D-60）。画面（エディタの入力項目の表・実行のダイアログ・ミニアプリの部品の候補）は `x-order` に書かれたキーをその順に先に置き、書かれていないキーをその後ろに置く（無ければ今の順）。エディタの表は書き戻すたびに今の並びを `x-order` に書く。
  - 起動の検証は `x-order` を注記として読み飛ばす。形（文字列の配列・同じオブジェクトの `properties` のキーだけ・重なりなし。全部のキーを書かなくてよい）は保存時検証が全階層で確かめ、合わなければ `ir.schema_violation` で止める（path は `/input_schema/x-order/<i>`・§8 V1）。
  - **保存時の形の検査**（GAP-35）: 起動の検証は、確かめられない形（上の集合に無いキーワード〔`$ref`・`oneOf` など〕・型の名前の誤り・空の `enum`・数でない範囲・読めない `pattern`・配列の `items`・形でない値）に当たると起動しない（fail-closed）が、見るのは値が通る位置だけで、省ける項目の下の誤りは値が入ったときに初めて起動を止める。そこで保存時検証が全階層を見て、同じ形を `ir.schema_violation` で止める（path は `/input_schema/properties/memo/oneOf` のようにスキーマの中の位置・§8 V1）。
  - **保存時の `pattern` の上限**（BUG-222・BUG-226）: 保存時検証は、`pattern` を 1 つ 256 文字（条件の `matches` の正規表現と同じ数・文字数で数える）まで・入力項目の全体で 50 個までにし、超えたものはコンパイルせずに `ir.schema_violation` で止める（長いものは `pattern は 256 字までにしてください。`、数は超えた最初の位置で 1 回だけ `pattern は、入力項目を合わせて 50 個までです。どれかの項目の pattern を減らしてください。`）。`llm.invoke` の `output_schema` の `pattern` も同じ決まりで、数はワークフローの全体（すべての `llm.invoke` の出力の形を合わせて）で 50 個まで数える（§7.3）。決まりは `ir::patterns` の 1 か所。だれでも呼べる検証の口（`POST /workflows/validate`）で、何万個もの `pattern` をコンパイルさせないため。保存のときだけの上限で、保存済みの版の起動の検証と実行は変えない。
  - **起動と出力の検証の `pattern`**（BUG-227）: 1 回の検証の中で、同じ `pattern` は一度だけコンパイルする（値ごと・配列の要素ごとにコンパイルし直さない）。

---

## 3. データフロー模型（式言語を持たない・重要決定）

**IR に自由記述の式言語（JS 式・CEL 等）を導入しない。** パラメータは「リテラル＋参照＋テンプレート＋構造化条件」のみで組み立てる。複雑な変換・計算はすべて script ノードへ昇格させる（すみ分け: **IR = 配線 / script = ロジック**）。

この決定の理由（[README.md](./README.md) の FAQ にも載せる）:

1. AI 編集・dnd の両方が「スキーマ検証可能な構造化データ」を生成する設計である（検証済みスペック方式）。文字列に埋め込んだ式は、パーサ・エスケープ・インジェクションの面が生え、保存時検証の網羅性が落ちる。
2. 認可語彙・参照の静的検証（ハルシネーション境界）を式の内部まで貫徹するコストが高い。
3. 逃げ道が既にある——script ノードは ms 級起動なので、条件計算に使ってもコストが軽い（[script.md](./script.md) §4）。

却下案: CEL（構文が TS 系と乖離・依存追加）/ JS ミニ式（結局 QuickJS が要る → script ノードと二重化）。

日付の計算（前日・先週・先月・月末・週の初め）も値の式に足さず、ブロック `transform.date`（日付を計算・§7.13）で行う（GAP-15）。
値の種類を 3 つのまま保ち、保存時検証・エディタの値の欄・codegen の型を増やさない。

パラメータ値として使える構造は 3 種——参照（`$from`）・テンプレート（`$template`）・構造化条件（`condition`）——である。以下順に定義する。

### 3.1 参照（`$from`）

パラメータ値の任意の位置に、リテラル JSON の代わりに参照オブジェクトを置ける。

```jsonc
{ "$from": "nodes.fetch_report.output", "path": "/items/0/name", "default": "untitled" }
```

- **source（`$from` の元）**:
  - `input` — run 入力（対話トリガの `input_schema` に沿う値）
  - `trigger` — トリガイベントのペイロード。手動の実行（`interactive`）では run の入力と同じ値になる（schedule のペイロードは `null`。予定の時刻は実行情報の `scheduled_at` で読む・GAP-15）。実行のダイアログ（`POST /workflows/{id}/runs` の `trigger`）では、ファイルやテーブルの変更をきっかけにするワークフローを試すために、きっかけの内容を入力と別に渡せる（項目のまとまりだけ・本人の権限で動くので、入力と同じく利用者が書いた値として扱う・D-93）。同じく `scheduled_at` で、スケジュールをきっかけにするワークフローを試す予定の日時を渡せる（時差つきの RFC 3339 か、時差の無い `2026-11-09T09:00`〔日本時間で読む〕・読めない値は `invalid_input`・`field = scheduled_at` の 400・GAP-88）
  - `nodes.<node_id>.output` — 他ノードの出力
  - `run` — 実行情報（読み取り専用・項目は vocab の `RUN_INFO_FIELDS` が正）: `date`（実行を始めた日・日本時間の `YYYY-MM-DD`）・`started_at`（実行を始めた日時・日本時間の RFC 3339）・`scheduled_at`（予定の日時・日本時間の RFC 3339）・`started_by`（実行した人）・`run_id`・`tenant_id`。日時は run を始めた時刻で決まり、同じ run の中ではやり直しても変わらない（ファイル名に差し込んでも冪等キーの照合が崩れない・D-97）。組織ごとのタイムゾーンの設定はまだ無いので日本時間で言う
    - `scheduled_at` は、schedule の run ではスケジュールの予定の時刻（`schedule_occurrence.scheduled_at` と同じ時刻。停止中の穴を埋める発火〔catchup〕や、多重度で待たされた run でも予定の時刻のまま）、予定の日時を指定した手動の実行（実行のダイアログの `scheduled_at`・実行履歴の「同じ入力でもう一度実行」は元の run の予定の日時を引き継ぐので、予定の日時から作るファイル名は元の run と同じになる・[engine.md](./engine.md) §11.4）ではその日時（GAP-88）、`workflow.start`（`Shiki.workflow.start` を含む）で始めた子の run では始めた run の予定の日時（親に予定の日時が無ければ子にも無く、子の `started_at` と同じ値になる。同じ run から始めた子どうしは同じ予定の日時になるので、予定の日時だけから作るファイル名は子どうしで重なる・GAP-90）、ほかの run（ふつうの手動の実行・event・この項目が入る前に作った run）では `started_at` と同じ値（GAP-15）。親に予定の日時があれば、子で同じ週・月を数えるのに親の `scheduled_at` を入力で渡さなくてよい。親に予定の日時が無い run（日時を指定しない手動の実行・event・ミニアプリ）が承認や待ちの後で子を始めると、子の「前日」「先週」は子の始まった日から数えるので親とずれる。親と同じ日を数えるなら、親で求めた日付（`transform.date` の結果）を入力で渡す。実行履歴の一覧と詳細（`GET /workflows/{id}/runs`・`GET /workflows/{id}/runs/{run_id}`）は、予定の日時のある run に `scheduled_at` と由来 `scheduled_at_source`（`schedule`・`specified`・`parent`・`rerun`。`rerun` は「同じ入力でもう一度実行」が元の run から引き継いだ日時）を返す（無い run は null・GAP-89）。「先週」「前日」を、どの実行でも同じ項目から数えられるようにする（`transform.date` の基準の既定・§7.13）。schedule の run の入力ときっかけの内容は今までどおり `null`（保存済みの定義の `$from input` / `$from trigger` の値を変えない）
    - `started_by` は、人が手動で実行した run（実行主体がユーザー。その run から `workflow.start` で始めた子の run を含む）の実行主体のユーザー ID で、テーブルのユーザーの列（`user_ref`）と比べられる（申請者の上長を承認者の表から引くなど・GAP-17・D-101）。schedule / event の run（ワークフロー主体）とミニアプリの自動化（サービスの主体）には人がいないので項目ごと入らず、参照すると `default` が無ければ `expr_resolve_error` で失敗する（黙って空の値で探さない）。入力項目で申請者を受け取る組み方は、実行する人が別の人の ID を入れて名乗れるので勧めない
  - `each` — **map 領域内のみ**。現在要素（`each.item` と `each.index`。§5.3）
- **`path`**: RFC 6901 JSON Pointer。省略時は source 全体を指す。
- **`default`**: 解決失敗（skipped ノードの出力・パス不在・null）時のフォールバック。**`default` がなく解決に失敗した場合、step は `expr_resolve_error` で失敗する**（黙って null を流さない）。実行時の解決アルゴリズムは [engine.md](./engine.md) §4。
  - **例外: 値式全体の null は値として通す。** パラメータの値（またはリテラルの object / array の要素）そのものが `$from` で、指した値が null のとき、`default` が無ければ null をそのまま渡す（項目を空にする書込などのため・D-35）。パスが無い・skipped ノードの出力は従来どおり失敗する。`$template` の差し込み（§3.2）の null は、空文字や `null` の文字を黙って埋め込まないよう `expr_resolve_error` で失敗する。
- **静的検証（§8 V5）**: `nodes.<id>` は**当該ノードの祖先**（DAG 上、いずれかのパスで先行するノード。map 領域の中のノードは、囲む map ノードの祖先も祖先に数える・§5.3・D-123）でなければ保存拒否。**すべてのパスで先行するとは限らない**祖先（分岐の片側にしかいない等）を参照する場合は `default` 必須（そのパスが取られないと skipped になり解決失敗するため）。
- **`secrets.*` という source は存在しない**。シークレットは参照名でノード設定に書き、エンジンが実行直前に解決・注入する（§7 `http.request` / [script.md](./script.md) §6）。値がデータフローに乗る経路を構造的に作らない。
- **エディタでの組み立て**（BUG-118）: 項目のまとまり（object）やリスト（array）を受ける欄では、エディタは値を「項目を並べる」（キーと値の行）と「リストを並べる」（1 件ずつの行）に分け、行ごとに値の入れ方（リテラル・`$from`・`$template`・さらに 1 段の組み立て）を選ばせる。行から作り直した値は開いた値と同じで、1 行を直してもほかの項目と参照は変わらない。組み立てるのは入れ子の 2 段（欄そのものとその中の 1 段）までで、それより深い位置の `$from` / `$template`、エンジンが値式として読めないオブジェクト（`$from` のキーがあるのに形が違うもの）、空のキーを持つ値は、組み立てずに JSON の欄で出して理由を言う（IR が置ける位置は §3.1 のとおり任意で、制限はエディタの表示だけ）。保存した IR のオブジェクトのキーの並びは残らない（IR は jsonb の列に保存され、jsonb はキーを長さと文字の順に並べ直し、読み出した値はキーの名前の順になる・BUG-156）ので、テーブルの列に書く値（`data.record.create` の `data`・`data.record.update` の `patch`）の行は列の並びで出す。ほかにも自然な順がある欄だけは、開いたときにその順で並べる（`workflow.start` で渡す入力 `input` は相手の `input_schema` の `x-order` の順・`workflow.reply` で直接書いた表の行は `columns` の順・D-128）。並べるのは画面だけで、保存する IR は変えない（並びのためだけに IR に順を持たせない）。

### 3.2 テンプレート（`$template`）

```jsonc
{ "$template": "{user} さんの経費申請（{amount} 円）が承認待ちです",
  "vars": {
    "user":   { "$from": "trigger", "path": "/record/applicant_name" },
    "amount": { "$from": "trigger", "path": "/record/amount" }
  } }
```

- **文字列組み立て専用**。`{name}` は `vars` のキーのみを参照できる（位置指定・ネスト・式は不可）。リテラルの `{` / `}` は `{{` / `}}` でエスケープする。
- 閉じの無い `{`（`a { b`）と対にならない `}` は文字のまま出す（エラーにしない）。`{name` の閉じ忘れは差し込んだつもりで差し込まれないので、閉じの無い `{` は保存時に警告する（`ir.template_unclosed_brace`・§8・D-37）。
- `vars` の値は参照（`$from`）またはリテラル。非文字列値は JSON 表現へ文字列化する（数値・bool はそのまま、object/array はコンパクト JSON）。
- 上限: `vars` の数 ≤ 50（§8 V7）。

### 3.3 構造化条件（`condition`）

`control.branch` ノード・イベントトリガの `filter`・`control.wait`（event）の `filter` で共通に使う条件木である。

```jsonc
{ "all": [
    { "op": "eq", "left": { "$from": "trigger", "path": "/record/status" }, "right": "submitted" },
    { "any": [
      { "op": "gte", "left": { "$from": "trigger", "path": "/record/amount" }, "right": 100000 },
      { "op": "eq",  "left": { "$from": "trigger", "path": "/record/category" }, "right": "travel" }
    ] }
] }
```

- **合成**: `all` / `any` / `not`（ネスト深さ最大 5・§8 V7）。ルートは合成（`all`/`any`/`not`）でも単一の op リーフでもよい。
- **op（閉じた集合）**: `eq` `neq` `lt` `lte` `gt` `gte` `in`（`right` = 配列リテラル）`contains`（string/array）`starts_with` `ends_with` `exists` `is_null` `matches`。
  - `matches`: 正規表現。**保存時にコンパイル検証する**（§8 V5）。Rust `regex` クレート（バックトラックなし）を用い、パターン長 ≤ 256（文字数で数える。入力項目の `pattern` と同じ・BUG-228）。
- **意味論は data 側と共通の表に従う**（2 値論理・左辺は値の集合として見る・`1 = 1.0`。表は [query-optimizer.md §3.1](../data/query-optimizer.md)、固定は共通の表 `semantics_table.json` を workflow の評価器・data の参照評価器・実 SQL にかけるテスト）。型の扱いは演算子で分かれる（D-34）。
  - **大小比較（`lt` `lte` `gt` `gte`）だけが型厳密**: 左右が数値どうし・文字列どうしでない組（数値と文字列・真偽値・オブジェクトなど）は false へ黙って落とさず、`expr_type_error` として step / トリガ評価を失敗させる（監査に残る。固定値の「100」と数値の 100 を比べて分岐が黙って逆に流れるのを防ぐ）。数値は serde_json の数値（i64/u64/f64）間でのみ数値比較する。値なし（未解決・null）と左辺が配列（多値）の大小比較は偽（表のとおり・多値の大小比較は data では使えない）。
  - **それ以外の演算子の型違いは偽**（失敗にしない）: `eq`・`in` は型が違えば等しくない（`neq` は真）。`contains`・`starts_with`・`ends_with` は文字列でない要素を満たさない。`matches` は左右が文字列でなければ偽。
- **暗黙変換なし**。truthy / falsy の概念を持たない（bool 以外を条件位置に置けない）。
- トリガ filter の評価エラーの実行時の扱い（発火しない＋監査）は [engine.md](./engine.md) §5。

---

## 4. ノード

ノードは処理単位である。1 ノードが 1 step の実行に対応する（map 領域では要素数分の step に展開・§5.3）。

```jsonc
{
  "id": "notify_manager",           // ^[a-z][a-z0-9_]{0,63}$・ワークフロー内一意
  "type": "notify.send",            // 閉じたノードカタログ（codegen 語彙・§7）
  "label": "上長へ通知",             // 表示用・任意
  "parent": null,                    // map 領域内なら親 map ノード id（§5.3）。領域外は null
  "params": { /* ノード種ごとのスキーマ。codegen が正・§7 */ },
  "retry": {
    "max_attempts": 3,
    "backoff": { "kind": "exponential", "base_sec": 5, "max_sec": 300, "jitter": true }
  },
  "timeout_sec": 30,                 // ノード種ごとに既定・上限あり（§7 の表）
  "on_error": "fail_run",            // fail_run（既定）| continue（error ポートへ）
  "cache": { "ttl_sec": 86400 }      // 任意・結果の使い回し（いまは ai.decide だけ・§7.11）。既定は使わない
}
```

- **`id`**: `^[a-z][a-z0-9_]{0,63}$`。ワークフロー内一意（§8 V2）。参照（`nodes.<id>`）・エッジ・`step_path`（[engine.md](./engine.md) §2）の識別子になる。
- **`retry`** の既定は `max_attempts: 1`（リトライなし）。**リトライは at-least-once を顕在化させる行為**なので、明示オプトインとする（PIT-31 の思想）。attempt を跨いで冪等キーは不変であり、その供給と dedupe の詳細は [engine.md](./engine.md) §7。
  - `backoff.kind`: `exponential`（`base_sec`・`max_sec`・`jitter`）。full jitter の実行は [engine.md](./engine.md) §7。
- **`timeout_sec`**: ノード種ごとに既定値と上限がある（§7 の表。正はノードカタログの `timeout_default_sec`・`timeout_max_sec`）。1 未満と上限超過値は保存拒否（§8 V7・`ir.limit_exceeded`・path `/timeout_sec`）。`agent.invoke` は `max_duration_sec`（省略は 600 秒）より短くできない。打ち切らない種（制御ノード・予約語彙）に書いた値は読まない（保存も止めない）。step timeout の実行時の扱い（retryable か等）は [engine.md](./engine.md) §9.4（BUG-96）。
- **`cache`**（step 属性・§7.8 で予約したもの）: 結果の使い回し。書けるのはノードカタログの `step_cache` がある種だけで、いまは `ai.decide` の決定キャッシュ（§7.11）。ほかの種に書くと保存時に `ir.cache_unsupported`。`ttl_sec` は 60〜2,592,000 秒（1 分〜30 日・省略は 86,400 秒＝1 日・範囲の外は `ir.schema_violation`）。答えは実行履歴の保持期間を超えて残らない（保持期間が短ければそちらが上限・§7.11）。キーは書けない（種が決める）。省略が既定で、使い回さない。
- **`on_error`**:
  - `fail_run`（既定）: エラーで run 全体を failed にする。
  - `continue`: ノードは出力ポート `error` を持ち、エラーオブジェクトが `error` ポートの下流へ流れる。

  `continue` 時に error ポートへ流れる形:

  ```jsonc
  { "error": { "code": "...", "message": "...", "retryable": true, "node_id": "...", "attempt": 2 } }
  ```

---

## 5. エッジ・ポート・グラフ制約

### 5.1 エッジ

```jsonc
{ "from": "check_amount", "from_port": "true", "to": "notify_manager" }
```

- `from_port` 省略時は `"out"`。
- **to 側にはポートがない**。合流・待ち合わせは `control.join` の**入エッジ集合**で定義する（下記 §5.4）。
- **グラフ制約（保存時検証・§8 V2）**:
  - **DAG**（閉路拒否）。自己ループ拒否。多重エッジ（同一の from / from_port / to）拒否。
  - **join 以外のノードの入エッジは最大 1 本**。合流したければ必ず `control.join` を置く。ダイヤモンド合流の実行セマンティクスが曖昧化するのを型で排除する（重要決定）。
  - すべてのノードは、いずれかのエントリノード（入エッジ 0 本のノード）から到達可能でなければならない（DAG ではどのノードもエントリから辿れる）。
  - **エントリノード（入エッジ 0）は複数可**。run 開始時に全エントリノードが ready になる（静的並列。[engine.md](./engine.md) §4）。
  - **孤立ノード**（本体のノードで、入るエッジも出るエッジも無いもの。本体に 2 つ以上ノードがあるとき）は保存を止めず、注意 `ir.unconnected_node` を出す（BUG-106）。孤立ノードもエントリなので実行の始めにほかのエントリと同時に動く。ドラッグで置いてつなぎ忘れたノードに気づかせるためで、意図して並べた入口もあるので誤りにはしない（保存済みの定義も保存し直せる）。map の領域の中は、出口ちょうど 1（§5.3）が孤立ノードを誤りにする。

### 5.2 ポートカタログ

| ノード種 | 出力ポート |
|---|---|
| 通常ノード | `out`（＋ `on_error: continue` 時 `error`） |
| `control.branch` | `true` / `false` |
| `control.switch` | 各 case 値ポート ＋ `default` |
| `control.join` | `out` |
| `control.map` | `out`（領域完了後・結果配列） |
| `control.wait`（event, `on_timeout: continue`） | `out` / `timeout` |
| `human.approval` | `approved` / `withdrawn` ＋ `rejected`（`reject` を指定したとき）・`returned`（`return` を指定したとき）・`timeout`（期限を決めて `on_timeout: continue` のとき）（§7.9） |
| `workflow.reply` | `out`（＋ `on_error: continue` 時 `error`）（§7.10） |
| `ai.decide` | `route` 無し＝`out`。`by: confidence`＝`out` / `uncertain`。`by: value`＝選択肢のキー（choice）か `true` / `false`（boolean）＋ `uncertain`（`out`・`default` は無い）（§7.11） |

- **ポートの決まり方はノードカタログの `port_rule` が単一定義**（`static`＝静的なポートだけ・`switch_cases`＝`cases[].port` と `default`・`approval_choices`＝`approved` / `withdrawn` に指定した判断と時間切れ・`decide_route`＝`route` から）。どの規則でも `on_error: continue` なら `error`、wait(event) で `on_timeout: continue` なら `timeout` が加わる。実際の集合は Rust の 1 つの関数（`workflow_engine::node_ports`）が params から導き、保存時検証 V2（§8）と検証 API（`POST /workflows/validate`）の応答の `node_ports`（ノード id → 出口の名前・表示の順）が同じ関数を使う。エディタはこの応答でハンドルを描き、導出を TS に書き写さない。カタログの `dynamic_ports` は互換のための印（switch と ai.decide で true）で、どう決まるかは `port_rule` を見る。
- params が契約に合わず出口を導けないノード（振り分けの質問が無い ai.decide など）は、V2 が出口を検査しない（V1 が報告する・同じ誤りを二重に出さない）。検証 API の `node_ports` にも載らない。
- ai.decide の出口は、出口に関わる項目（`route` と、その質問の種類・選択肢のキー）だけから導く。ほかの質問の誤り（書きかけの問い）・モデルの未選択・しきい値の範囲では導けなくしない（エディタが古い出口を今の出口として描き、利用者が無い出口へつながないように）。導けないのは、`route` の質問が無い・同じキーの質問が複数ある・段階（score）の質問・選択肢のキーが出口の名前にできない（形・予約名・重なり・件数）ときだけ。
- `control.switch` の case ポート名は params の `cases` で宣言する値と一致させる（保存時にポート存在検証・§8 V2）。
- `control.switch` の `value` が解決できない（`default` の無い `$from` のパスが無いなど・§3.1）ときは、`default` ポートへ流さず `expr_resolve_error` で失敗する。どの case にも一致しないのと、値が無いのを区別する（黙ってその他へ流さない・D-36）。「値が無ければその他へ」としたいときは、`$from` に `default` を書く（例: `{ "$from": "input", "path": "/kind", "default": "" }`）。

### 5.3 map 領域（動的 fan-out）

`control.map` は `items`（配列への参照）を受け、**領域（region）**を要素ごとに並列実行する。

- **領域** = `parent: "<map_id>"` を持つノード集合。制約（§8 V2）:
  - 領域内ノードのエッジは**同一領域内で閉じる**（領域跨ぎのエッジは保存拒否）。
  - 領域の**入口** = 領域内で入エッジ 0 本のノード（複数可）。
  - 領域の**出口** = 領域内で出エッジ 0 本のノード**ちょうど 1 つ**（出口の出力が要素結果になる）。
  - map の**ネスト可・最大深さ 2**。
  - **中にノードの無い領域は保存を止めない**（エディタで置いた直後の map・D-85）。保存時の注意 `ir.empty_region` を出し、有効化と対話の起動の前に止める（§8 の保存を止めない警告・§9）。入口・出口の制約は、中にノードのある領域だけに掛ける。ネストの深さ（最大 2）は中が空の領域にも掛ける（後から中に置いた時点で突然 `ir.region_too_deep` にしない）。それをすり抜けて要素のある空の領域を実行したときは、map の step を `empty_region`（設定の誤り・やり直さない）で失敗させ、要素ごとの出口を待ち続けない（[engine.md](./engine.md) §4.5）。
- **`params`**:

  ```jsonc
  { "items": { "$from": "nodes.list_files.output", "path": "/items" },
    "max_concurrency": 10,           // 領域の要素並列度（初期値 10）
    "on_item_error": "fail_map" }    // fail_map（既定）| collect
  ```

  - `fail_map`（既定）: 1 要素の失敗（リトライ枯渇後）で map ノード自体が失敗する（`on_error` に準拠）。実行中の他要素は完走させる。
  - `collect`: 失敗要素は `{ "error": ... }` として結果配列に混ぜ、map は成功扱いにする。
- **map 出力**:

  ```jsonc
  { "items": [ /* 要素結果…（入力順） */ ], "errors": [ { "index": 3, "error": { /* … */ } } ] }
  ```

- **fan-out 上限**: 要素数 ≤ 1000（初期値）。超過は step 失敗・`fanout_limit_exceeded`。
- 領域内ノードから現在要素を参照するには `each`（`each.item` / `each.index`・§3.1）を使う。`step_path` 上の map 要素表現は [engine.md](./engine.md) §2。
- **領域の外の結果**（D-123）: 領域内ノードは、map ノードの祖先（エッジでたどれる前のノード）の出力も `nodes.<id>.output` で参照できる。ネストした map の中では、外側の map ノードの祖先（本体のノード）と、内側の map ノードの祖先（外側の領域のノード）の両方をたどる。領域内ノードが動くのは map ノードが動いた後なので、その祖先は終わっている（承認者の表を引いてから 1 件ずつ書く、など）。map ノード自身（要素がすべて終わるまで出力が無い）と、map の後ろのノードは祖先ではない。実行時は、本体の出力・自分の要素の出力・それを囲む外側の要素の出力を見せ、ほかの要素の出力は見せない（[engine.md](./engine.md) §4.5）。

#### map 領域の構造例

```mermaid
flowchart LR
  subgraph OUTER["ワークフロー本体"]
    LIST["list_files<br/>(storage.list)"] --> MAP["map_files<br/>(control.map)<br/>items=list_files.output/items"]
    MAP -->|out: 結果配列| DONE["notify_done<br/>(notify.send)"]
  end
  subgraph REGION["map_files 領域（parent = map_files・要素ごとに複製実行）"]
    direction LR
    IN["parse<br/>(script.run)<br/>入口: 入エッジ0"] --> STORE["store<br/>(storage.write)"]
    STORE --> OUT["summarize<br/>(script.run)<br/>出口: 出エッジ0・ちょうど1つ"]
  end
  MAP -.->|要素ごとに領域を展開<br/>each.item / each.index| IN
  OUT -.->|要素結果| MAP
```

### 5.4 join

`control.join` は複数入エッジの待ち合わせ点である。

- **`params`**: `{ "mode": "all" | "any" }`（既定 `all`）。
- **`all`**: 全入エッジが「解決」するまで待つ。解決 = 生きて完了（live）または死んだ（dead・§5.5）。
  - 出力: `{ "<from_node_id>": <output or null(dead)>, ... }`
  - 全入エッジが dead なら join 自体が skipped。
- **`any`**: 最初の live 解決で発火（1 回だけ）。
  - 出力: `{ "winner": "<node_id>", "output": ... }`
  - 残る分岐は完走するが、その下流で join(any) は再発火しない。全 dead なら skipped。全 live が失敗（`fail_run` でない場合）なら failed。
- join の**発火判定の実行時セマンティクス**（どの遷移でどう ready 化するか）は [engine.md](./engine.md) §4 が正本。本節は静的定義（出力形・mode の意味）を規定する。

### 5.5 skip 伝播（静的定義）

skip 伝播の**実行アルゴリズム**（どの TX でどう dead を伝播させるか）は [engine.md](./engine.md) §4 が正本。本節はエッジ状態と skipped の**定義**のみ規定する。

- **エッジ状態**: `pending` → `live`（源が成功しポートが取られた）/ `dead`（源が skipped、または源は完了したが当該ポートが取られなかった、または源が失敗して `error` 以外のポート）。
- ノードは「入エッジがすべて解決し、かつ live が 1 本以上」で ready になる（join 以外は入エッジ ≤ 1 なのでこの規則は自明）。入エッジが（唯一の 1 本が、または join なら全てが）dead ならノードは `skipped` になり、その全出力ポートのエッジが dead になる（連鎖）。
- run の終了判定: 全ノードが terminal（succeeded / failed / skipped）になったら run terminal（実行時判定は [engine.md](./engine.md) §4）。

---

## 6. トリガ

```jsonc
[
  { "id": "t_sched", "kind": "schedule", "cron": "0 9 * * MON", "tz": "Asia/Tokyo", "catchup": "skip" },
  { "id": "t_event", "kind": "event", "source": "data.transition",
    "filter": { "cmp": { "left": { "$from": "trigger", "path": "/to" }, "op": "eq", "right": "approved" } },
    "scope": { "table": "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1" } },
  { "id": "t_manual", "kind": "interactive" }
]
```

- トリガは `nodes` とは別セクションに置く（dnd 上は擬似ノードとして描画する）。ペイロードは `{ "$from": "trigger" }` で参照する。
- 複数トリガ間でペイロード形が異なる場合は、`default` または `control.branch` で吸収する（保存時に警告・§8）。

### 6.1 `schedule`

- cron 5 フィールド ＋ `tz` 必須（IANA タイムゾーン名）。
- **曜日は標準の読み方**: 0 と 7 が日曜、1 が月曜〜6 が土曜。名前（`SUN`〜`SAT`・大小文字は問わない）も書け、数・範囲（`1-5`・`MON-FRI`）・列挙・刻み（`*/2`・`1-5/2`。始まりだけの `1/2` は 7 まで）を混ぜてよい。範囲の逆転（`5-1`・`SAT-SUN`）・8 以上・`L` や `#` は保存時に `ir.bad_schedule`（BUG-95）。日と曜日の両方を指定したときは**両方に合う日**だけに動く（標準の cron の「どちらか」とは違う）。
- 保存時の検証・発火・エディタの「次の実行」（`POST /workflows/schedule-preview`）は同じ関数（`workflow_engine::scheduler::cron`）で読む。エディタは曜日を名前で書く。
- **DST**: 「存在しない時刻はスキップ・二重時刻は先勝ち」。
- **予定の時刻**: 発火した run は、その occurrence の時刻を予定の日時として持つ（`$from run` の `scheduled_at`・§3.1）。ペイロード（`$from trigger`）と run の入力は `null` のまま（GAP-15）。
- **`catchup`**:
  - `skip`（既定）: 停止中の未発火は直近 1 回だけ発火する。
  - `none`: 停止中の未発火は捨てる（稼働中の時刻どおりの発火はする。判定は engine.md §5.2）。
  - **`all`（全部再生）は v1 でやらない**。
- misfire（停止中の穴）の埋め方の実行時挙動は [engine.md](./engine.md) §5。

### 6.2 `event`

- **`source`** は閉じた集合 `storage.write` / `data.record.created` / `data.record.updated` / `data.transition`（既存 outbox イベント種に対応）。将来追加は codegen 語彙で行う。加えて `event.custom`（`event.publish` ノードが発行するカスタムイベント）と `webhook.received`（外部システムからの webhook 受信・署名検証とレート制限は受信エンドポイント側で設計）を**予約済み**（§7.8・Stage A では保存不可）。
- **`scope`** で対象（テーブル・フォルダ）を**必須束縛**する。全イベント購読を許さない。フォルダ / テーブル id は保存時に存在検証する（§8 V4）。形は source ごとに決まる（§8 V3）:
  - `storage.write`: `{ "folder": "<uuid>" }`（祖先束縛・配下のフォルダの保存でも起きる）。
  - データ変更の 3 種: `{ "table": "<uuid リテラル（小文字・ハイフンつき）>" }` だけ。`$from`・空・他のキーは拒否する。保存ビュー単位の scope は v1 では入れない（同じ絞り込みは `filter` で書く）。
- データ変更のトリガは `declared_scopes` に `data.read` が要る（保存の入口で補う）。有効化者はトリガのフォルダ・テーブルの viewer であること（有効化時に確かめる）。
- `filter`（条件木・§3.3）でペイロードを絞る。filter 評価エラーの実行時扱い（発火しない＋監査）は [engine.md](./engine.md) §5。
- **ペイロード**（run 入力・`$from trigger`）の形は source ごとに決まり、**語彙のカタログ（`EVENT_CATALOG` の `payload_fields`・`crates/workflow-engine/src/vocab/event_catalog.rs`）が正**（エディタの参照の候補と AI の説明文も同じ表から作る・D-47）:
  - `storage.write`: `{ file_id, version, op（create | update）, kind（"file"）, parent_id, size, actor, blob_sha256 }`。ファイル名・content_type は載せない（`storage.read` の出力から取る）。`parent_id` は保存した時点の親フォルダ（作成でも新しい版でも載る）。
  - データ変更: `{ source, table_id, record_id, rev, change（create | update | transition）, from, to（状態が変わったときだけ・ほかは null）, changed（変わった列の参照名）, changes（列ごとの { old, new }）, actor, by_workflow（ワークフローの書込による変更か）, occurred_at, record（今の行）, truncated }`。値は**有効化者（委譲元）の見え方**で作る: 委譲元に見えない行の変更・読めない列だけの更新では発火しない（記録もしない）。読めない列は `changed`・`changes`・`record` に載らない。64KB を超えると `record` と `changes` を落として `truncated: true`。
- `storage.write` は**ファイルの作成**（op `create`）と**新しい版の保存**（op `update`: 利用者のアップロード・内容の更新（Office・ノートの保存・`csv.patch`）・提案の採用・上書き）で起きる。削除・移動・名前の変更・版の復元・フォルダの作成では起きない。新しい版の保存は**同じファイルの続く保存を 1 回にまとめる**: 同じトリガ・同じファイルの保存が 60 秒途切れてから、最後の版のペイロードで 1 回だけ起動する（保存が途切れなくても、まとめ始めから 10 分で起動する・D-38）。作成はまとめずにすぐ起動する。発火の時点で workflow プリンシパル（有効化の委譲）が保存されたファイルを読めなければ発火しない（記録もしない・D-50）。まとめて待たせた発火も、起動する時点で読めなくなっていれば起動しない（BUG-50）。このワークフロー自身の保存とその子（`workflow.start`）の保存では発火しない（下のデータ変更と同じ因果の鎖・D-26）。
- データ変更のトリガは次では発火しない（[engine.md](./engine.md) §5.4）: 一括取込（CSV の取込・`data.bulk_upsert` の作成行と更新行）、このワークフロー自身の書込とその子（`workflow.start`）の書込（因果の鎖）、鎖が 4 段を超える連鎖、queued の run が 1,000 件以上あるとき。状態の変更は行の更新でもあるので、同じテーブルに `data.record.updated` と `data.transition` を置くと 1 回の遷移で 2 回起動する（保存時に警告 `ir.overlapping_data_triggers`）。
- filter の例: 遷移先 `/to eq "approved"`・変更した列 `/changed eq "amount"`（配列の eq は「含む」）・変更前後の値 `/changes/amount/new gte 100000`・今の値 `/record/data/category`・人の操作だけ `/by_workflow eq false`。
- 起動に一時失敗して再試行を待った発火（最長 20 分ほど）は、起動する時点の委譲元の見え方で変更を作り直してから起動する（BUG-75・ファイル保存の BUG-50 と同じ考え方）。待つ間に委譲元が行を見られなくなった・委譲が外れたら起動しない（`skipped`）。読めなくなった列は run 入力に載らず、`/record` はその時点の行になる。作り直した変更が filter に合わなくなったら起動しない（run 入力はいつも filter に合う）。

### 6.3 `interactive`

- UI ボタン・チャットからの起動。エンベロープ直下の `input_schema`（§2）が入力フォーム契約になる。

### 6.4 実行主体

実行主体はトリガ種別で決まる（[miniapp-platform.md](../miniapp-platform.md) §2.3 のまま。強制は [engine.md](./engine.md) §6）:

| トリガ種 | 実行主体 |
|---|---|
| `interactive` | トリガした本人 |
| `schedule` / `event` | workflow プリンシパル（`workflow:<tenant>|<id>`）＋有効化時の明示委譲 |

---

## 7. ノードカタログ v1（params の正は codegen）

**ノード params のスキーマは Rust 型定義から codegen（JSON Schema ＋ TS 型）するのが正**であり、本表は**設計仕様**として用途・冪等性区分・スコープ・timeout を規定する（右パネル UI の契約は codegen 型が駆動する・Task 10.6/10.12）。

**冪等性区分**:
- `engine-dedup` — 内部チョークポイントが冪等キーで重複排除し、副作用が高々 1 回になる（実行の詳細は [engine.md](./engine.md) §7）。
- `best-effort` — 外部副作用。exactly-once を約束しない（UI とドキュメントに明示する）。
- `pure` — 副作用なし。

| type | 区分 | 概要 | 冪等性 | timeout 既定/最大 |
|---|---|---|---|---|
| `storage.read` | 能力 | 文章のファイルの中身と、PDF・Office の本文を読む（≤ 10MB。文章の種類で文字コードを決められない中身・文章でない中身は `not_text_file` で失敗・BUG-101。PDF・Office など文章でない種類は、社内文書の検索に取り込まれた本文を先頭から 100,000 字〔`STORAGE_READ_MAX_DOCUMENT_CHARS`〕まで読み、原本は読み直さない〔下の「PDF・Office の本文」・D-111〕。文字コードは BOM〔UTF-8・UTF-16LE・UTF-16BE〕→ 全体が妥当な UTF-8 → 日本の文字コード〔Shift_JIS（CP932）・EUC-JP〕の推定の順で決め、推定の確からしさが低ければ断る。結果に読んだ文字コード `encoding` を載せ、`text` には BOM を含めない・GAP-45。種類は、拡張子が文章の種類〔.txt・.md・.csv・.tsv・.json・.log など〕なら MIME より拡張子を優先して中身の確かめに任せ〔ブラウザーが付けた MIME をそのまま保存しているため。Windows の .csv の `application/vnd.ms-excel` など〕、それ以外は MIME で決め、MIME が無いか `application/octet-stream` のときだけ拡張子で決める〔.dot・.key のように文章にも使う拡張子では断らない〕。結果に読んだ中身のバージョン `version` を載せる・D-96） | pure | 30s / 120s |
| `storage.write` | 能力 | 新しいファイルの保存（既存のファイルは書き換えず、新しいバージョンも足さない。同じ名前があれば `on_name_taken` に従い、省略・`fail` は `name_taken` で失敗、`add_number` は「名前 (2).md」〜「名前 (50).md」の空いている名前で保存する。`content_type` を省くと拡張子〔.txt・.md・.csv・.json・.html〕から決め、決まらなければ `application/octet-stream`・BUG-97・D-97。種類が `text/csv` のときだけ `encoding`〔`utf8_bom`・`utf8`・`shift_jis`〕の文字コードで書く。省略は BOM なしの UTF-8〔この項目が無かったころの定義の挙動〕で、エディタで新しく置くブロックは `utf8_bom`。ほかの種類では使わない・D-120） | engine-dedup | 30s / 120s |
| `storage.list` | 能力 | フォルダ一覧（名前の順で最大 100 件〔`STORAGE_LIST_MAX_ITEMS`〕。後ろに見えるものがあるときは、その 100 件を `items` に入れ、`truncated` を true にし、`next_cursor` に続きの位置を入れる。ちょうど 100 件なら `truncated` は false・`next_cursor` は null。有無は 1 回の読み込み〔上限 + 1 件目まで探す・権限の確認 1 回・監査 1 行〕で決める・BUG-180。省略できる `cursor`〔ValueExpr〕に前の結果の `next_cursor` を渡すと続きを読む。省略・null・空の文字は先頭から〔この項目が無かったころの定義の挙動〕。続きの位置はテナントと一覧〔フォルダー・並び〕に束縛した不透明な文字列で、読めない位置・別のテナント・別のフォルダーの位置は `invalid` で失敗する。OpenFGA に問う数の上限〔1 回の読み込みで 1,000 問〕に届いたときは、`items` が 100 件より少なくても `truncated` が true になり、続きが空で終わることもある・BUG-187・D-131） | pure | 30s / 120s |
| `csv.query` | 能力 | CSV と Excel（.xlsx・.xlsm）に読み取り専用の SQL を実行して集計・抽出する（隔離ランナー。CSV の文字コードは `storage.read` と同じ判定で決めて UTF-8 にしてからランナーへ渡し、決められなければ `not_text_file`・GAP-45。Excel は先頭のバイト〔zip〕で決めてそのままランナーへ渡し、ランナーが展開して表示している左端のシート〔隠したシートは飛ばす〕を `data` として読む。ほかのシート〔隠したシートも〕は、SQL に `FROM "集計"` のように書くとシートの名前の表として読める〔SQL が表として読む名前のシートだけを読む。名前は大文字と小文字を区別しないので、`data` と同じ名前のほかのシートと、前のシートと大文字と小文字だけが違う名前のシートは名前で読めない。値の無いシートは行の無い表。左端のシートに値が無いときは、ほかのシートを名前で読めることを言って `not_text_file`・GAP-84〕。列の名前は、表の上の表題の行を飛ばした最初の行〔値の数が、すぐ下の 20 行の値の数の中央の値の半分以下の先頭の行を表題とみなす。名前の決め方は CSV の読み込みと同じ・GAP-83。限界: 見出しの半分以上が空の表では本当の見出しを表題として飛ばし、データの 1 行目が列の名前になって結果から 1 行減る。値が 2 つの表題（表の名前と作成日など）は、3 列以下の表の上では飛ばさない。どの行を見出しにしたかは列の案内の見出しの行の番号で確かめられる〕で、値の無い行は飛ばす。数式は計算せず保存された値を読み、外部のブックへの参照はたどらない。ファイル 50 MB・zip の中の数 10,000・展開した大きさ 1 つ 64 MB／合わせて 256 MB・SQL が読むシートを合わせた値 100 万個／値の文字の合計 64 MB／表の大きさ〔値のある行の数 × 列の幅〕1,000 万セル・シートごとの列の幅 1,000 を超える Excel と、古い形式〔.xls〕・パスワードを付けたファイルは `not_text_file`・D-17。結果に集計した中身のバージョン `version` を載せ、後ろの `csv.patch` の `base_rev` にできる・D-96。結果の `rows` は値をテキストで〔空は null・日付と日時は `2024-01-31`・`2024-01-31 09:30:00` の形〕、`items` は 1 行ずつ `{列の名前: 値}` で数値・はい／いいえ・空を種類のまま返す〔JavaScript で正確に扱えない大きな整数と非数はテキスト〕。`column_types` は結果の列の型〔DuckDB の型の名前〕、`total_rows` は打ち切らなかったときの結果の行の数〔打ち切ったときは null〕。結果は 10,000 行か、大きさ 8 MB〔`rows` と `items` の 1 行ずつを JSON にした大きさの合計〕を超える行の手前で打ち切り、`truncated` を立てる〔1 行目が超えるときは行の無い結果・GAP-92〕。SQL の末尾のセミコロンと `--` の注釈は許す・GAP-40） | pure | 30s / 120s |
| `csv.patch` / `csv.write` | 能力 | CSV のセル・行・列を書き換えて新しいバージョンを保存する／新しい CSV を保存する（同じ名前があるときは `storage.write` と同じ `on_name_taken`。`csv.patch` は元の文字コードと BOM のまま書き戻し、元の文字コードに無い文字は書かずに `invalid` で失敗する・GAP-45。`csv.patch` の区切りと改行は `csv.query` の読み込みと同じ推定〔隔離ランナーの `sniff_csv`〕で決め、同じ区切りと改行で書き戻す〔Excel の「Unicode テキスト」のタブ区切り・推定できなければカンマ区切り・BUG-144〕。`csv.write` は `encoding`〔`utf8_bom`・`utf8`・`shift_jis`〕の文字コードで書き、省略は BOM なしの UTF-8〔この項目が無かったころの定義の挙動〕、エディタで新しく置くブロックは `utf8_bom`。`shift_jis` で書けない文字は書かずに `invalid` で失敗する・D-120。`csv.write` の `content` と、種類が `text/csv` の `storage.write` の `content` は、文字ならそのまま書き、表〔`csv.query` の結果 `{columns, rows}`。行は値のリストか、列の名前で引く項目のまとまり〕と項目のまとまりのリスト〔列はすべての項目の名前を合わせたもの・名前の順〕は CSV に組み立てる。区切りはカンマ・行の終わりは CRLF・カンマ／二重引用符／改行を含むセルは二重引用符で囲む〔RFC 4180〕。先頭が `=` `+` `-` `@` タブ CR の 2 字以上の文字で数の表記でないセルは、先頭に `'` を付けて表計算ソフトの式にしない〔監査ログの書き出しと同じ打ち消し方〕。数・真偽・空の値・表でない項目のまとまり・まとまりでないもののリスト・入れ子の値・`csv.query` が打ち切った表〔`truncated: true`〕は、JSON の文字のまま保存せずに `invalid` で失敗する・BUG-178。Excel は扱わない: `csv.patch` に Excel などの CSV でないファイル〔zip・古い形式の Office〕を渡すと、`csv.write` の `name` が `.xlsx`・`.xlsm`・`.xls` で終わると、どちらも `invalid` で失敗する・D-17） | engine-dedup | 30s / 120s |
| `data.query` | 能力 | 宣言的クエリ（**data の `DataQuery` をそのまま使う**・比較値に `$from` 可・`query.limit` は 1〜200 で範囲の外は保存時に `ir.schema_violation`・BUG-108） | pure | 30s / 120s |
| `data.record.create` / `data.record.update` / `data.record.delete` | 能力 | レコード書込（rev 楽観ロック・rev 省略時は実行時点の rev。`data.record.update` は省略できる `add`〔数の列 → 足す数〕で、いまの値に足して書く・§7.12・GAP-16） | engine-dedup | 30s / 120s |
| `data.record.upsert` | 能力 | 同じレコードを探す列（`match_on`・重複を許さない列）の値が同じレコードがあれば書き換え、なければ追加する（`add` で数を足せる・出力はレコードと `created`・§7.12・GAP-16） | engine-dedup | 30s / 120s |
| `data.transition` | 能力 | **FSM 遷移 API を叩く**（直接 status 書込は構造的に不可能） | engine-dedup | 30s / 120s |
| `data.bulk_upsert` | 能力 | 一括の作成・更新（data の一括書込パス・最大 5,000 行・誤りは行ごとに拒否。省略できる `match_on` で、`id` の無い行を同じ値のレコードに当てて更新にする・§7.12・GAP-16） | engine-dedup | 600s / 600s（切れると途中まで登録したまま失敗するので、既定でも最大まで待つ・D-106） |
| `rag.search` | 能力 | permission-aware 検索（必要スコープは既存語彙 `rag.query`。結果の各件に関連の強さ `relevance`〔high・medium・low。reranker の値を設定のしきい値で段階に落としたもの〕を載せる。`score` は順位から決めた値〔1/(順位+1)〕で関連の強さではない・D-96） | pure | 60s / 120s |
| `notify.send` | 能力 | アプリ内通知 | engine-dedup | 30s / 60s |
| `workflow.start` | 能力 | 別ワークフロー起動（fire-and-forget・run_id 返し。相手は保存時に V4 で照合し、実行時に無い・見えない相手は同じ `not_found`〔permanent〕で失敗する・BUG-107。相手の最新の保存内容を、親の実行主体の権限で動かす・D-91） | engine-dedup（起動 1 回保証） | 10s / 30s |
| `control.branch` / `switch` / `join` / `map` / `wait` | 制御 | §5 | pure | ―（wait は別枠・§7.5） |
| `script.run` | script | shiki script 実行 | best-effort（script 次第・冪等キー供給） | 30s / 360s |
| `skill.invoke` | skill | `skill:<name>@<version>` | skill 宣言に従う | 60s / 360s |
| `llm.invoke` | AI | llm-gateway 直行 | pure（課金あり。結果を効果の記録に残し、リースが切れて拾い直したときは呼び直さない・[engine.md](./engine.md) §7.3・GAP-28） | 120s / 600s |
| `agent.invoke` | AI | agent-core のツールループ（ループはホスト側・道具は能力 API・コードの実行の道具だけをサンドボックスで実行・§7.4） | best-effort | 600s / 3600s |
| `ai.decide` | AI | 判定用のモデルで、質問に決められた値域（選択肢・段階・はい/いいえ）で答え、確からしさを付けて返す（llm-gateway の判定のチョークポイント・§7.11） | pure（課金あり） | 60s / 300s |
| `http.request` | 外部 | 外部 HTTP | **best-effort**（Idempotency-Key 注入支援。送り先の状態を変えうる送信〔GET・HEAD・OPTIONS 以外〕で相手が誤り〔400 以上〕を返さなかったものは結果を効果の記録に残し、リースが切れて拾い直したときは送り直さない。送っている途中で落ちた送信は送り直さずに `http_delivery_unknown` で止まり、再開で送る。届いたが応答が大きくて記録していない送信はやり直さない・[engine.md](./engine.md) §7.3・§7.4・GAP-28） | 30s / 120s |
| `human.approval` | 人間参加 | テーブルの 1 行について担当者の承認・却下・差し戻しを待って分岐する（判断は承認者本人の FSM 遷移・§7.9） | engine-dedup（台帳は step で 1 件） | 30s / 120s（停止まで。判断を待つ時間は `timeout_sec` と `run_timeout_sec`） |
| `workflow.reply` | フロー連携 | 結果を画面に返す（ボタン・会話から起動した本人の画面に出す・スコープ不要・§7.10） | pure | 5s / 10s |
| `transform.date` | データ加工 | 日付を計算する（基準の日をずらし、その日を含む日・週・月・年の最初と最後の日を返す・スコープ不要・§7.13・GAP-15） | pure | 5s / 10s |

以下、設計上の要点を持つノード種を個別に規定する。params の網羅的スキーマは codegen 型が正であり、本節は制約と出力形を示す。

- **PDF・Office の本文**（`storage.read`・D-111）: 文章でない種類（MIME か拡張子で決める。判定は上の表の文章のファイルと同じ）は、
  社内文書の検索が取り込んだ本文（版のブロック列。無ければ検索用の断片）を、先頭から 100,000 字まで 1 つにつないで読む。
  - 結果は文章のファイルと同じ項目（`file_id`・`id`・`name`・`version`・`content_type`・`size`・`text`）に、上限の字数で切ったかの
    `truncated`（boolean）を足したもの。`text` にはページの変わり目の印（`〔p.2〕`）と見出しの行（`#` で始まる）が入り、`encoding` は
    無い。`version` は読んだ本文の内容のバージョン（名前の変更・移動でファイルのバージョンが進んでも、本文は取り込んだ内容のもの）。
    文章のファイルは切らないので、`truncated` はいつも false。
  - 認可はチャットの `doc_read` と同じ契約: 閲覧の判定（StorageService）と、検索と同じ可読集合での本文の authz_tags の照合（pre-filter）の
    両方を通す。見えない・無い・pre-filter で読めないは同じ `not_found`（存在の秘匿。文章のファイルも同じ）。要るスコープは
    `storage.read` のまま（本文はファイルの中身から作ったもので、読める範囲は広がらない）。
  - いまの内容（内容のバージョンの履歴の最新）の本文が索引に無いときは、待てば読めるかで分ける: 取り込みへ向かう記録（書き込みと
    同じ Tx で積まれた outbox の未処理のイベント・取り込みの待ち行列・取り込みの記録）があり、その取り込みがまだ終わっていない・
    取り込み中・一時的な失敗のやり直し待ちなら `index_pending`（retryable）。ごみ箱から戻した直後（戻した取り込みがまだ記録に無い）も
    ここに入る。`index_pending` はエンジンが試す回数（`retry`）を使わずに、間隔を伸ばしながら上限（30 分）まで待って読み直し、
    過ぎたときだけ step の失敗にする（engine.md §7.5 の `wait`・D-125・GAP-50）。取り込めない種類（画像・.doc・.xls など）・取り込みの上限より大きい・本文の無い文書・取り込まない場所（AI エージェントの
    作業用のフォルダー＝システム領域・`node.system`）のファイルは `not_indexable`、取り込みに失敗した（やり直しを使い切った）もの・
    取り込みへ向かう記録がどこにも無いもの（社内文書の検索を有効にする前に置いた・取り込みを飛ばした）は `index_failed`（ファイルを
    編集できる人がドライブのメニューの「検索に取り込み直す」を選ぶと取り込まれる・D-126。保存し直しても取り込まれる）、社内文書の
    検索が使えない環境では `search_disabled`（失敗の分類は「この環境で使えない機能」・BUG-158。以前は `not_text_file`）
    （いずれも permanent）。
    新しい内容の取り込みが終わるまでは、古い内容の本文を読まない。
  - script の `Shiki.storage.read` は文章のファイルだけを読む（いまのまま）。文章でない種類は `not_text_file` で断り、前に
    「ファイルを読む」ブロックを置いてその結果を script の入力に渡す組み方を、script の作者向けに言う（BUG-158）。
- **ファイルの ID の項目名は `file_id` にそろえる**（D-96）: `storage.read`・`storage.write`・`csv.write`・`csv.patch` の結果は `file_id` を載せ、古い名前（`storage.*` の `id`・`csv.*` の `node_id`）も同じ値で残す（保存済みの定義の `$from` を壊さない）。`rag.search` の結果・ファイル保存のトリガのペイロードはもとから `file_id`。出力の形の正はカタログの `output_fields`。
- **集計の行の形**（`data.query`・GAP-132）: `query` に `aggregate` を書くと、結果は `groups` に入る（`items` は無い）。1 行は
  data の `AggregateGroup` をそのまま出した `{ key: { まとめた列の参照名: 値 }, value: 先頭の指標の値, values: [指標の値（aggregate
  と同じ順）] }` で、ノードで詰め替えない。まとめた列の値は列の型によらず文字（値の無いものは null）、件数（`count`）は整数、ほかの
  指標は数。件数が少なくて伏せた行（PIT-17）は `value` と `values` の値が null。結合先の列でまとめたときの `key` の名前は
  `別名.参照名`。`control.map` で回すときは、領域の中で、まとめた列を `{"$from": "each", "path": "/item/key/<参照名>"}`、指標を
  `/item/values/<aggregate の位置（0 から）>`（先頭は `/item/value`）で読む（`/item/<参照名>`・`/item/count` は解決できず
  `expr_resolve_error`）。AI の `emit_workflow` の説明文はこの指し方を教える。

**出力の形の正はノードカタログ**（`crates/workflow-engine/src/vocab/catalog.rs`）の `output_fields`（最上位の項目の名前・型・省略されうるか・補足）である。AI の説明文に載せる TS 風の表記（`output_shape`）はそこから作り、エディタの参照の候補も `output_fields` を読む（表記を字句解析しない・D-42）。設定で決まる項目は、その設定の場所（params の JSON Pointer・`present_if`）を持つ（`llm.invoke` の `data` は `/output_schema`、`data.query` の `count` は `/query/with_count`、`groups` は `/query/aggregate`）。エディタは、その場所に値があるときだけ参照の候補に出す（値の入らない候補を選ばせない・BUG-108）。項目の組でない出力（入力をそのまま通す・script の戻り値など）は文で書き、項目は空にする。

### 7.1 `data.transition`（FSM 遷移）

**status を進めたいときも必ず通常の遷移 API を叩く**（[miniapp-platform.md](../miniapp-platform.md) §1）。エンジンに status を直接書かせて FSM の不変条件を破ることは構造的に不可能——このノードは遷移 API を呼ぶだけであり、直接 status 書込のノードは存在しない。ガード・認可は遷移 API 側で再評価される。

```jsonc
{ "id": "approve", "type": "data.transition",
  "params": {
    "table": "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1",   // テーブル id（UUID・$from 可）
    "record": { "$from": "trigger", "path": "/record_id" },
    "to": "approved",              // 遷移先の status（FSM は「現在の status → to」で遷移を定義する）
    "expected_rev": { "$from": "trigger", "path": "/rev" }   // 省略時は実行時点の rev
  } }
```

- 出力: 遷移後のレコード（実行主体のフィールドマスク適用後）。
- テーブルは FSM で管理するもの（`fsm_ref` のあるもの）に限る。リテラルで書いたテーブルは保存時に照合する（§8 V4・BUG-105）。
- Task 15.7 で実装した際、遷移名（`transition`）と遷移入力（`input`）の指定は採らなかった。data の FSM
  （`FsmBody`）は遷移に名前を持たず「from → to」で定義し、遷移に入力も取らないため。
- **data ノード共通（Task 15.7）**: 認可は data 側（行述語・フィールドマスク・OpenFGA）。interactive run は
  起動したユーザーの権限、schedule / event run は**ワークフロー主体に委譲されたテーブルだけを、委譲元
  ユーザーの権限で**読み書きする（行は委譲元の行述語で絞られる・[engine.md](./engine.md) §6.8）。

### 7.2 `http.request`（外部 HTTP・PIT-36 の型防御）

```jsonc
{ "id": "post_slack", "type": "http.request",
  "params": {
    "method": "POST",
    "url": "https://api.slack.com/api/chat.postMessage",  // ホスト部はリテラル必須
    "path_suffix": { "$from": "input", "path": "/channel_path", "default": "" },  // パスは可
    "query": { /* $from / $template 可 */ },
    "body": { "$template": "…", "vars": { /* … */ } },
    "secret": { "name": "slack-bot-token", "attach": { "kind": "bearer" } },
    "headers": { "X-Request-Id": { "$from": "input", "path": "/request_id" } },  // 名前はリテラル・値は $from / $template 可
    "redirect": "deny",            // deny（既定・いまはこれだけ。follow_stripped は未実装）
    "fail_on_error_status": true   // 相手が 400 以上を返したら失敗にする（省略は false）
  } }
```

- **URL のホスト部は IR 上リテラル必須**（`$from` / `$template` 不可。パス・クエリ・ボディは可）。保存時に egress allowlist・シークレット宛先束縛と照合できるのはこの制約のおかげであり、PIT-36 の動的 URL 迂回を型で殺す（§8 V4）。
- **`secret`**: `{ "name": "<参照名>", "attach": { "kind": "bearer" | "header", "header"?: "X-Api-Key" } }`。参照名のみを書き、解決・宛先束縛検証・注入はエンジンが実行直前に行う（値がデータフローに乗らない・§3.1）。`attach.header` は下の `headers` と同じ決まりで、保存時（V1）と実行時に名前を確かめる（Shiki が決めるヘッダー・HTTP の名前の形でないものは断る・BUG-184）。
- **`headers`**（省略可・GAP-19）: 作成者が決めるヘッダー（名前 → 値）。名前はリテラルで、値は `$from` / `$template` を書ける（値は文字にする）。送れるヘッダーの決まりと上限は `Shiki.http.request` と同じ（`nodes/http_headers.rs`・[script.md](./script.md) §6.7）: Shiki が決めるヘッダー（`Host`・`Content-Length`・`Transfer-Encoding`・`Connection` ほか hop-by-hop・`Accept-Encoding`）・シークレットを付けるヘッダーと同じ名前・形の誤り・制御文字・重複（大文字と小文字は区別しない）・20 個／値 4 KB／合計 8 KB を超えるものは、保存時に分かる範囲は V1 で、値が実行時に決まるものは実行時に `invalid` で断る。`Idempotency-Key`・`Content-Type` を書くと、Shiki が付ける値の代わりにそれを送る。省略はこの項目が無かったころの定義の挙動。書いた値は IR（版の履歴を含む）に残り、ワークフローを見られる人に見え、応答に映り返っても隠さない（隠すのはシークレットの値だけ）。鍵を入れることの多い名前（名前に `authorization`・`api-key`・`token`・`secret`・`password`・`cookie`・`session` などを含む・`nodes::http_headers::looks_like_key_header`）のヘッダーに、IR に書いたままの値（リテラルの文字と、`$template` の差し込み以外の文字。`Bearer` などの認証の方式の語だけは除く）があると、保存を止めない注意 `ir.plain_key_header`（`/params/headers`・注意の文に値は映し返さない）を返す（GAP-19）。鍵は `secret` で付ける。
- **`redirect`**: 既定 `deny`。いまは `deny` だけで、`follow_stripped`（追従時にシークレットを剥がして再検証する・PIT-36）は未実装（IR の `RedirectPolicy` に無いので保存で断り、`Shiki.http.request` は `invalid` で断る・GAP-27）。
- **実行時の注入はコネクション検証後**: ホスト部リテラル制約は動的 URL 迂回を塞ぐが、DNS rebinding（検証後に解決先 IP が付け替わる）は塞げない。エンジンが名前解決・コネクション確立・検証を行い、**検証済みコネクションにのみ**シークレットを注入する（PIT-36 ③。実行時の正は [engine.md](./engine.md) §7.4・`Shiki.http.request` と同一実装）。名前解決の後のアドレスが内部のネットワーク（リンクローカル・クラウドのメタデータを含む）なら、シークレットの有無によらず送らない（BUG-181・D-135）。
- 応答: `{ "status", "host", "body": /* JSON なら parse・そうでなければ text・≤ 1MB */ }`（`host` は照合した送り先）。応答の `headers`（許可リストのみ）は未実装で、いまは返さない（GAP-27）。
- 冪等性は best-effort。`Idempotency-Key: <冪等キー>` ヘッダの注入支援のみ提供する（[engine.md](./engine.md) §7）。
- **`fail_on_error_status`**（省略は false・BUG-99・D-95）: true なら、相手が 400 以上を返したときブロックを失敗にする。400〜499（408・425・429 を除く）は `http_error_status`（permanent）、500 以上と 408・425・429 は `http_retryable_status`（retryable・`max_attempts` の範囲でやり直す。408 と 425 は同じ要求を後で送り直せば通りうる）。省略・false は誤りの応答も成功として `status` を返す（この項目が無かったころの定義の挙動）。エディタで新しく置くブロックは true で置く。送ったことは失敗にするときも監査に残す。
- **送れる先**: シークレットを使わない送信は、デプロイ設定 `workflow.http_allowlist` の宛先だけ（既定は空で、空ならシークレットなしの送信はすべて拒否する）。シークレットを使う送信は、そのシークレットの宛先束縛の中だけで、`http_allowlist` を設定していればその中にも限る（`nodes/capability_ai.rs` の `check_http_destination`）。
  - どちらも、名前解決の後のアドレスがすべて公開の IP のときだけ送る（D-135）。内部の IP へ送れるのは、管理者がデプロイ設定 `workflow.http_internal_hosts`（内部の送り先・`host` か `host:port`・ワイルドカードは書けない）にそのホストを書いたときだけ（オンプレの社内システムを呼ぶ使い方）。`http_allowlist` に書いただけでは、そのままのホストでも内部へは送らない（許可は公開の SaaS を書く場所でもあり、DNS を持つ人が解決先を内部へ向けると内部の任意のポートへ届く・PIT-48 の ②）。内部の送り先は名前解決の結果をそのまま信じるので、DNS を管理者が管理するホストだけを書く。送れる先の照合（上の `http_allowlist`・宛先束縛）は別にかかる。リンクローカルとクラウドのメタデータのアドレスは、書いても送らない。断った送信は `destination_internal`・`destination_link_local`（permanent）で失敗する（[engine.md](./engine.md) §7.4）。送り先の名前のアドレスが無い（NXDOMAIN など・URL のドメインの誤り）ときは `destination_not_found`（permanent・やり直さない・BUG-202）、DNS に届かない・一時的に引けないときは `unavailable`（retryable）。外へ出るのに中継が要る環境では、運用者がデプロイ設定 `egress.proxy_url` に書いた中継を通す（確かめた IP アドレスへの CONNECT・中継が断ったら `egress_proxy_refused`・[engine.md](./engine.md) §7.4・GAP-72）。

### 7.3 `llm.invoke`

```jsonc
{ "id": "classify", "type": "llm.invoke",
  "params": {
    "model": "…",                  // 省略可。省略すると組織の既定（Auto を含む）。指定時は組織が許可したモデルへ保存時照合＋実行時再照合
    "effort": "low",               // 省略可。low | medium | high（思考の深さ）
    "prompt": { "$template": "…", "vars": { /* … */ } },
    "max_tokens": 2048,            // 省略可（gateway の既定）
    "output_schema": {             // 省略可。書くと構造化出力（出力に検証済みの data が付く）
      "type": "object",
      "required": ["category", "summary"],
      "properties": {
        "category": { "type": "string", "enum": ["billing", "bug", "howto", "cancel", "other"] },
        "summary":  { "type": "string", "maxLength": 200 }
      } }
  } }
```

- `model` を省略すると（空文字も同じ）、**実行のたびに**実行主体の組織（tenant_id ＋ org）の AI 設定で解決する。チャットと同じ規則（llm-gateway の `resolve_choice`）で、既定が Auto なら Auto の振り先と思考強度を使う。本人専用のモデル（ChatGPT プラン）は、実行主体によらず候補から外す（誰が起動しても同じモデルになる）。
- `model` を指定すると、保存時に組織が許可したモデル（本人専用を除く）と照合する（§8 V3）。実行時にも同じ規則で照らし直し、使えなくなっていれば組織の既定へ戻さずに失敗する（[engine.md](./engine.md) §6 の fail-closed）。
- 組織がモデルの選択を許していないときは、既定と異なる `model` を `ir.model_choice_disabled` で拒否する（実行時も同じ規則で失敗する）。
- 保存時の照合と編集画面のモデルの欄（`GET /llm/models?purpose=workflow`）は、**保存する人（編集する人）の利用者ごとのプロバイダの制限**を通す（D-169）。その人に許されないプロバイダのモデルは欄に並べず、書いた `model` は `ir.model_unavailable_for_user` で拒否する（組織で使えないモデルの `ir.unknown_model` と言い分け、一覧から選び直すよう言う）。`model` を省いたノードも、実行時に動く組織の既定（既定が Auto なら Auto の振り先）がその人に使えなければ同じコードで拒否する（D-185。組織がモデルの選択を禁じていれば、ほかを選べないので管理者に相談するよう言う）。実行の主体は制限しない（schedule・event の実行はワークフローの主体で、制限されたプロバイダのモデルでも動く）ので、制限された人が自動実行で制限を回り込む入口は保存で閉じる。照合は保存した時点のもので、後で組織の既定や制限が変わっても、保存済みの版は照らし直さない（開くことはでき、保存し直すときに照らす）。
- `effort` は思考の深さ（`low` / `medium` / `high`）。省略すると組織の既定（既定が Auto なら Auto の思考強度）。書くと組織の既定より優先する。ただし、組織がモデルの選択を禁じているときと、`model` を省略して組織の既定の Auto で動くときは使わず、組織の設定に従う（チャットと揃える・D-75。チャットも Auto では利用者の指定を使わない）。選択を禁じた組織で、既定と同じ `model` を明示したノードは、省略と同じに解決する（使うモデルは同じで、思考強度は組織の既定。選ぶ画面の無いチャットと同じ）。そのときは保存時に `ir.effort_ignored` で警告する（保存は止めない）。実行時の規則は llm-gateway の `resolve_workflow_choice` で、警告も同じ条件で出す。思考に対応しないモデルでは gateway が送らない（[design.md](../design.md) §4.5）。
- 出力は `{ "text", "model" }`。`model` は実際に使ったモデルの論理名で、会計・監査にも同じ名前を残す。
- `output_schema` を書くと構造化出力になり、出力は `{ "text", "model", "data" }` になる。`data` は形で検証済みの値で、`text` はモデルが返した JSON の文字列。後続は `/data/<項目>` で読む（分類の値を script で解析しなくてよい）。
- **`output_schema` の制約**（保存時に V1 が検査する。誤りは `ir.schema_violation`・path は `/params/output_schema/...`）:
  - キーワードは `input_schema` と同じ集合（§2）。`$ref`・`anyOf`・`oneOf` などは書けない。リテラルだけで、`$from` は書けない。
  - 根は `type: "object"`。各スキーマは JSON のオブジェクトで書く（`true` / `false` は書けない）。
  - オブジェクトは閉じる。`additionalProperties` は `false` か省略で、**省略は閉じたものとして扱う**（`input_schema` の「省略は開いている」とは逆。実行時は `false` を書き足した形で検証し、プロバイダへ送る）。
  - `type` の配列は `[T, "null"]` だけ（1 つの型と null の組）。
  - プロパティ名は `^[A-Za-z_][A-Za-z0-9_]{0,63}$`。`required` の名前は `properties` にあり、重ならない。`required`・`additionalProperties` は object の項目にだけ、`items` は array の項目にだけ書ける。
  - `pattern` はコンパイルできること。保存のときは 1 つ 256 文字まで・ワークフローの全体（すべての `llm.invoke` の出力の形を合わせて）で 50 個までにし、超えた分はコンパイルしない（数は超えた最初の位置で 1 回だけ `出力の形の pattern は、すべての「AI に聞く」を合わせて 50 個までです。どれかのブロックの pattern を減らしてください。`・§2 の入力項目と同じ決まり・BUG-226）。実行のときの形の確かめ（保存を通らない経路の IR を緩く送らない多層防御）は上限を掛けない（保存済みの版の実行を変えない）。`enum` は空でなく、値は文字列・数値・真偽・null。
  - 上限: 入れ子の深さ 5（根から `properties` / `items` をたどる段数）・プロパティ 100・enum の値 256・JSON 16 KiB・任意の項目（`required` に無いもの）24・null を許す項目 16。数はどれも入れ子を含めた全体。任意の項目と null を許す項目の上限は、Anthropic の構造化出力の上限（1 回の要求の全スキーマの合計）に合わせた。
  - 数値の範囲・文字列の長さ・`pattern`・配列の件数など、プロバイダの制約付きデコードが受けない制約は、gateway が送る前に落として説明文に書き足す。正しさは実行時の事後検証で守る。
  - 項目の並びは注記 `x-order`（§2・D-60 と同じ形。どの階層のオブジェクトにも書ける）が持つ。gateway は `x-order` のキーをその順に先に、書かれていないキーを保存された順（辞書順）で後ろに置いてプロバイダへ送る（制約付きデコードは送った順に書くので、「理由」を「結論」より先に置けば理由から書かせられる・D-103・PIT-82）。`x-order` そのものは送らない。エディタの表は書き戻すたびに今の並びを `x-order` に書く。
- **構造化出力の実行**（`workflow_engine::nodes::llm_structured`）:
  1. 答えが上限トークン（`max_tokens`）で切れたら `llm_output_truncated`、モデルが回答を拒否したら `llm_refused` で失敗する。どちらも直させない。
  2. 本文を JSON として読み（前後のコードフェンスは剥がして読み直す）、形で検証する。
  3. 合わなければ 1 回だけ直させる。直す点は最大 5 件・各 200 字までを伝え、会話は user で終える（直前の出力を assistant に置き、直しの依頼を user に置く）。
  4. 直しても合わなければ `llm_output_invalid`。プロバイダがスキーマを受け付けなかったとき（複雑すぎる・使えない書き方）も `llm_output_invalid` で、直させない。
  - 3 つの失敗はどれも再試行しない（step の `retry` で作り直し続けない）。`on_error: "continue"` なら error の出口へ進む。
  - 修復の呼び出しの会計のキーは `#c1`、使えなかった呼び出しは `#f{uuid}`（[engine.md](./engine.md) §7.2-7.3）。
- `output_schema` を書いて `max_tokens` を 1024 未満のリテラルにすると、保存時に警告する（`ir.output_schema_few_tokens`。思考の分も上限に数えるので、JSON の途中で切れやすい）。
- `output_schema` を書かない自由な本文は従来どおり: 上限で切れても拒否されても、そのときの本文で成功する。
- `temperature` は IR に持たない。決定性は、構造化出力で値域を縛ること（enum）・モデルの版を固定すること・checkpoint（成功した step は再開しても再実行しない）で担保する。

### 7.4 `agent.invoke`

```jsonc
{ "id": "review", "type": "agent.invoke",
  "params": {
    "instruction": "契約書を doc.read で読み、社内のチェックリストと照らして要注意の条項を挙げる",  // 自然文の依頼
    "context": { "$from": "input" },              // 参考データ（指示ではない）・省略可
    "allowed_tools": ["doc.read", "rag.search"],  // 道具の閉集合（codegen の AgentTool）・省略は道具なし
    "model": "…",                                 // 省略は組織の既定（§7.3 と同じ）
    "max_steps": 8,                               // 1〜24
    "max_tokens": 100000,                         // この実行全体の合計
    "max_duration_sec": 600,
    "egress_allowlist": ["docs.example.com"]      // web.fetch の宛先だけ・管理者の天井の部分集合
  } }
```

- **agent-core のツールループを step から 1 回走らせる**（チャットのスレッドは作らない・step が唯一の耐久単位・承認者を置かない）。依頼は自然文で、モデルは道具で調べながら作業し、最後の回答を出力の `text` に書く。依頼をコードとして実行しない（コードの実行は道具 `code.run`）。
- **道具（`allowed_tools`）は能力 API の名前の閉集合**（codegen の `AgentTool`・説明・入力の形・要求スコープ・結果の上限を単一定義にする）。

  | 道具 | 中身 | 要求スコープ |
  |---|---|---|
  | `rag.search` | 権限を考慮した社内検索（1 件 1,500 字・20 件まで） | `rag.query` |
  | `storage.list` | フォルダの一覧（100 件まで・続きがあると `truncated` が true で `next_cursor` に続きの位置が入り、同じ `folder_id` と一緒に `cursor` に渡すと続きを読む・BUG-180・D-131） | `storage.read` |
  | `storage.read` | テキストのファイルを窓で読む（`offset`・`max_chars` ≤ 20,000・続きは `next_offset`）。PDF・Word などは `doc.read` へ誘導する | `storage.read` |
  | `doc.read` | 索引済みの文書の本文を窓で読む（権限なしと不在は同じ応答・索引が無ければその旨） | `doc.read` |
  | `web.search` | 公開 web の検索（**検索語が社外の検索サービスへ出る**・10 件まで） | `http.egress` |
  | `web.fetch` | ページの取得（**社外へ出る**・宛先は下記・解決後の IP を検証しリダイレクトを追わない） | `http.egress` |
  | `code.run` | 隔離環境で Python を実行（外部への通信は遮断・成果物は保存しない・出力 16KB まで） | `sandbox.exec` |

  `data.list_tables` / `data.query` / `data.get` は予約（V3 `ir.unknown_tool`・DataPort の委譲台帳を通す形で後から足す）。**書込の道具は無い**（書き込みは後段の能力ノードで行う）。
- **省略は道具なし**（有効化済みの定義の到達範囲を再同意なしに広げない）。保存時は**明示した道具の分だけ**要求スコープを `declared_scopes` に補う。同意計画は、社内を読む道具（`rag.search`・`storage.*`・`doc.read`）にフォルダの閲覧の委譲を提案する（範囲は有効化者が選ぶ）。
- **縮小のみ**: 実効権限 = 実行主体 ReBAC ∩ `declared_scopes` ∩ 提示した道具の要求スコープ。道具の呼び出しは、エンジンの能力ゲートウェイ（提示した道具か → scope 天井 → 引数の上限 → 宛先 → 監査 via=agent）を通ってからチョークポイント（ReBAC・二段 authz・存在秘匿）へ届く（[engine.md](./engine.md) §6.5）。ノード設定で ReBAC 以上の権限は生えない（[miniapp-platform.md](../miniapp-platform.md) §2.5）。
- **`egress_allowlist` は web.fetch の宛先にだけ効く**。管理者が決める天井（デプロイ設定 `workflow.agent.web_allowlist`）の部分集合でなければならない（V3 `ir.host_outside_ceiling`）。天井が未設定・空なら web.fetch は提示しない（`workflow.http_allowlist`＝http.request の宛先には戻さない。http.request に広い宛先を許した環境で、AI の web.fetch まで同じ宛先を継いで間接プロンプトインジェクションの出口を広げないため・D-59）。web.fetch を選んだら空にできない（`ir.missing_egress`）。書き方はホスト名か `*.example.com`（`ir.bad_host`・20 件まで）。IR の作者と実行者が別になる使い方（共有・画面からの起動）で、作者が選んだホストへ実行者のデータを持ち出させないための天井である。
- **上限**: `max_steps`（既定 8・最大 24）・`max_tokens`（この実行全体の合計・デプロイ設定が天井・既定 200,000）・`max_duration_sec`（10〜3600・既定 600）・1 回の実行の費用（デプロイ設定）。ノード設定は縮めることしかできない。道具の結果にも上限を掛ける（1 件 32,000 字・切ったら目印）。
- **出力**: `{ "text", "complete", "stop", "model", "citations", "tool_calls", "skipped_tools", "usage" }`。
  - `stop` は `completed` / `truncated` / `budget_steps` / `budget_time` / `budget_tokens` / `budget_cost`。上限で止まっても本文があれば成功（`complete: false`）、本文が無ければ失敗（`agent_incomplete`・何の上限かと直し方を文言に書く）。同じ道具の失敗を繰り返したら失敗（`agent_loop`）。
  - `citations` は道具で読んだ資料（社内は `file_id`・`file_name`、web は `url`・`title`。50 件まで）、`tool_calls` は `{ tool, ok }` の列（100 件まで）、`skipped_tools` は指定したが提示しなかった道具（予約・scope 天井の外・web の天井が空・この環境で未配線）。
- `mount_scope`・`output_schema` は効くようにしてから足す（効かない設定を契約に入れない）。
- **旧い定義**（`instruction`・`egress_allowlist`・`max_duration_sec` だけ）は `ir_version` を上げずにそのまま読み、`instruction` を自然文として扱う（旧い実装は Python として実行し、自然文は必ず失敗していた・§9）。出力の形は `{ stdout, exit_code }` から変わった。
- 道具 `code.run` の**隔離ティアは admin ポリシー `chat.sandbox_backend`（既定 gVisor・#346）**（ノード単位のティア選択ノブは出さない・[design.md](../design.md) §4.6）。

### 7.5 `control.wait`

```jsonc
{ "id": "wait_manager", "type": "control.wait",
  "params": {
    "kind": "event",               // duration | until | event
    "source": "data.transition",   // kind=event 時
    "scope": { "table": "expense" },
    "filter": { /* 条件木・§3.3 */ },
    "timeout_sec": 259200,
    "on_timeout": "continue"       // fail | continue（timeout ポートへ）
  } }
```

- `kind`: `duration`（相対時間）/ `until`（絶対時刻）/ `event`（イベント待ち）。
- `on_timeout: continue` のとき出力ポート `timeout` を持つ（§5.2）。
- `kind: event` の `scope` は必須（D-49）: `storage.write` は `{ "folder": "<uuid>" }`（ファイルの作成と新しい版の保存の両方で起きる。トリガと違い、続く保存をまとめず最初に届いた保存で起きる）、データ変更は `{ "table": "<uuid リテラル>", "record"?: 値式 }`。`record` は**停止時に解決**し（解決できなければ `expr_resolve_error`）、その行の変更だけを待つ。データ変更の source では空の scope をワイルドカードにしない。保存済みの scope の無い `storage.write` の wait は、起床のたびに実行主体の閲覧を確かめて守る（`EventWaitGate`・D-33）。データ変更の wait は `data.read` が要る。
- **登録より前の変更では起きない**: 行に束縛した wait は停止時に実行主体の権限でその行を読み（data.get）、その rev より後の変更だけで起きる。テーブル単位の wait は停止の時点の outbox の位置より後のイベントだけを見る（停止と並行していた書込は取りこぼし得る）。停止の時点から待ち始めるまでの間に入った変更も、待ち始めた直後の 1 回の突き合わせで拾う（[engine.md](./engine.md) §9.2・BUG-42）。
- **filter の `$from`**: `event` が届いたイベントを指す（`trigger` / `payload` は互換の別名で、保存時に警告 `ir.wait_filter_alias`）。それ以外（`input`・`nodes.*`・`run`・`each`）は**停止時に解決してリテラルへ束縛**してから購読に保存する（起床の判定は届いたイベントしか解決しない）。出力（`nodes.<id>.output`）は届いたイベントのペイロード（形は §6.2 と同じ・実行主体の見え方）。
- wait の実行時挙動（waiting_timer / waiting_event・購読登録・起こし方）は [engine.md](./engine.md) §9。

### 7.6 `script.run`

```jsonc
{ "id": "compute", "type": "script.run",
  "params": {
    "source": { "inline": "function main(input) { return { total: input.a + input.b }; }" },
    // または { "artifact": "script:<name>@<ver>" }
    "input": { "$from": "trigger" }
  } }
```

- `source`: `{ "inline": "…" }`（≤ 64KB・保存時にコンパイル検証・§8 V6 / [script.md](./script.md) §3）または `{ "artifact": "script:<name>@<ver>" }`（version 固定なので保存時は存在検証のみ）。
- 実行契約（入出力・at-least-once・冪等キー）は [script.md](./script.md) §7。

### 7.7 `skill.invoke`

- `skill:<name>@<version>` を参照する。保存時にレジストリ＋インストール済みを存在検証する（§8 V4）。実効 = skill 宣言スコープ ∩ 実行主体 ReBAC（[miniapp-platform.md](../miniapp-platform.md) §4）。

### 7.8 将来予約ノード（語彙予約・現ステージでは保存不可）

後方互換性を最初から固定するため、将来ノードの **type 名（＝IR の `"type"` 値・TS 型・UI カタログ）と要求スコープを閉集合に先行予約**する（human 確定・issue #180）。予約 variant は `NodeType::available_stage_a()` が false を返し、保存時検証 V3 が「Stage A では未対応」として拒否する。**実装時に variant 追加は発生しない**（available 集合への追加＋実装のみ＝後方互換）。params スキーマは実装フェーズで codegen 定義する（本表は用途と認可方針の予約）。

| type | 系 | 概要 | 要求スコープ（予約） |
|---|---|---|---|
| `control.loop` | 制御 | 条件ループ（DAG にサイクルを持ち込まずエンジン側で展開） | ─ |
| `control.assert` | 制御 | 条件不成立で run を失敗させるアサーション | ─ |
| `debug.log` | デバッグ | 実行履歴への構造化ログ出力（pure） | ─ |
| `transform.template` / `parse` / `serialize` / `regex` / `map` / `filter` / `reduce` | データ加工 | 宣言的加工群（テンプレート展開・CSV/XML/JSON パース/直列化・正規表現・コレクション操作）。script.run でも書けるがノーコード UX の一級ノード。全て pure | ─ |
| `sheet.read` / `sheet.write` / `sheet.append` | オフィス | スプレッドシート操作（セル・範囲・シート・数式は params で表現＝中粒度） | `sheet.read` / `sheet.write`（append は write） |
| `doc.read` / `doc.edit` / `doc.comment` | オフィス | 構造化ドキュメント操作（見出し・表・コメント） | `doc.read` / `doc.write`（edit/comment は write） |
| `memory.get` / `memory.set` | 状態 | 会話・履歴・ユーザー状態ストア（テナント/ユーザー/ワークフロースコープ） | `memory.read` / `memory.write` |
| `event.publish` | イベント | カスタムイベント発行（購読はトリガ `event.custom`＝§6.2。フロー途中の待ち合わせは `control.wait` の `kind=event` 拡張で対応し新ノードは作らない） | `event.publish` |
| `workflow.call` | サブフロー | **同期**サブフロー呼び出し（結果を待って受け取る）。fire-and-forget は既存 `workflow.start` | `workflow.start`（流用） |
| `llm.embed` | LLM | テキスト埋め込み（llm-gateway 経由） | ─（天井対象外） |
| `llm.extract` | LLM | スキーマ指定の構造化出力（出力スキーマ検証まで契約に含む） | ─（天井対象外） |
| `ai.review` / `ai.eval` | AI | コード/文章/表のレビュー・LLM 出力品質評価（LLM-as-judge）。構造化出力（指摘リスト・スコア）の契約を持つ | ─（天井対象外） |
| `ai.ocr` / `ai.image.analyze` / `ai.image.generate` / `ai.transcribe` / `ai.speech` | AI | モダリティ拡張（OCR・画像認識/生成・音声認識/合成）。文書処理基盤（Docling）との役割分担は実装時に設計 | ─（天井対象外） |
| `graphql.query` | 外部 | GraphQL 呼び出し（クエリ・変数の型付き UX）。gRPC はユースケースが薄く予約しない | `http.egress`（流用） |
| `sandbox.exec` | 実行 | サンドボックス内の単発コマンド/Python 実行（secure-exec 基盤を転用。サンドボックス由来出力は敵対的として扱う・PIT-23） | `sandbox.exec`（危険度が高く独立スコープ） |

`human.approval` は GAP-08 で実装した（§7.9）。データ加工の系には `transform.date`（日付を計算）を GAP-15 で足して実装した（§7.13・予約の表のほかの `transform.*` は予約のまま）。`doc.read` と `sandbox.exec` の**スコープ**は agent.invoke の道具（`doc.read`・`code.run`）のために解禁した（同意画面では「文書の本文を読む」「隔離された環境でプログラムを動かす」と出す）。ノードは予約のまま。

**ノードにしない**と確定したもの（予約対象外・理由の明文化）:

- **Retry / Timeout / Try-Catch / Fallback** — ノード**属性**（`retry` / `timeout_sec` / `on_error`＋error ポート）として §4 に存在済み。
- **Cache / Rate Limit / Mutex（同時実行制御）** — 任意ノードに付く **step 属性**として予約（`cache`（LLM/API 結果のキャッシュ・キーと TTL）・`rate_limit`（API 呼び出し制限）・`mutex`（排他キー））。DAG を汚さず全ノードに合成可能で、追加は params/IR フィールド追加＝後方互換。
  - **`cache` は `ai.decide` の決定キャッシュとして確定した**（§4・§7.11・D-52 の H6）。形は `{ "ttl_sec": <秒> }` だけで、キーは種が決める（作者は選べない）。既定は使わない。付けられる種はノードカタログの `step_cache`（期限の既定・下限・上限）が正で、それ以外の種は `ir.cache_unsupported`。生成（`llm.invoke`）には付けない（同じ入力でも別の文章を期待されることがあり、答えの形も決まらない）。
- **Transaction** — **補償（compensation）エッジ**として予約。失敗時に補償ノードへ逆順で流す saga 型（分散トランザクションは提供しない）。
- **Secrets（API キー管理）** — ノードではなく参照（`http.request` の secret 参照＋宛先束縛・§7.2）。OAuth/JWT/API Key 管理も同じ connection/secret 基盤で扱う。
- **SQL 直接実行** — **作らない**。生 SQL は二段 authz・FSM 不変条件をバイパスし得るため、構造化データは宣言的な `data.query` に限定する。
- **通知のサービス別ノード（Slack/Teams/メール等）** — 作らず `notify.send` の params（`channel`・connection 参照）に統合する。サービス追加が語彙追加にならない。
- **Parallel / Merge** — DAG のファンアウト＋ `control.join` で表現済み。**Break Point** はエディタ/実行 UI の機能であって IR 語彙ではない。

### 7.9 `human.approval`（承認待ち・レコード束縛）

```jsonc
{ "id": "manager_ok", "type": "human.approval", "label": "上長の承認",
  "params": {
    "title": "経費申請の承認", "instructions": "規程どおりか確認してください。",
    "record": { "table": "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1",
                "id": { "$from": "trigger", "path": "/record_id" } },
    "assignees": [ { "field": "approver" }, { "field": "deputy" }, { "role": "keiri" } ],
    "approve": { "to": "承認", "label": "承認する" },
    "reject":  { "to": "却下", "comment": "required" },
    "return":  { "to": "差し戻し", "comment": "required" },
    "show_fields": ["title", "amount", "used_on"],
    "timeout_sec": 259200, "on_timeout": "continue" } }
```

- **レコード束縛が必須**: 承認は FSM で管理するテーブルの 1 行に束縛する（`record.table` は UUID のリテラル・`record.id` は値式で停止時に解決）。
- **判断は承認者本人の FSM 遷移**: 担当者が受信箱（`/approvals`）で判断すると、承認者本人の権限で行を `approve` / `reject` / `return` の `to` へ遷移する（`FsmStore::transition`）。**誰が承認できるかの正は FSM の actor 述語 1 か所**で、テーブル画面・ミニアプリ・別のワークフローから同じ遷移をしても同じ承認が決まる。代理承認は actor 述語（`any(approver eq $user.id, deputy eq $user.id)`）で表す。OpenFGA の relation は増やさない。そのため、述語が申請者を除いていなければ、申請者は自分の申請を承認できる（承認者の列を書き換えられるなら、自分を承認者にしてテーブル画面から承認できる・BUG-110）。防ぐには、述語に申請者の列との `ne $user.id` を入れ、承認を待つあいだ申請者が承認者の列を書き換えられないよう、行の書き込みの述語（`row_policy.write`）を状態で絞る。エディタの承認者の欄の注記はこのことを言う。
- `assignees`（20 件まで）は**受信箱に出す相手**だけを決める（権限の正ではない）: `{ "user": 値式 }`・`{ "role": 値式 }`・`{ "field": "<列>" }`（束縛した行のユーザー・部署の列・停止時に実行主体の権限で読む・読めなければ失敗）。
- `title`（120 字まで）・`instructions`（2000 字以内）はリテラルだけ（行の値を受信箱へ写さない）。受信箱に出す中身は、担当者本人の権限で行を読み直して作る（`show_fields` は 30 列まで・担当者に読めない列は出ない）。`show_fields` に状態の列は選べない（受信箱は状態を別に出し、出すのは状態が停止時のままの行だけなので、値はいつも同じ・保存時 V4）。
- `approve` は必須、`reject` / `return` は省略可。`to` は互いに異なる FSM の状態（保存時 V4）、`comment` は `required` / `optional`。
- **置いたばかりの承認**（D-129）: `title`・`record.table`・使う判断の `to` が空の文字、`assignees` が 0 件（または値のある担当者がいない）、担当者の `field` が空の文字でも、保存は通す（§8 の `ir.required_text_empty`）。実行と自動実行のオンの前に止める（§9）。保存時の照合（V4）は値があるときだけ行う（空の `to` を「状態「」はありません」と言わない・BUG-84）。エディタは置いたばかりの承認をこの空の形で置く。
- `timeout_sec`（リテラル・`policies.run_timeout_sec` 未満・V7）と `on_timeout`（`fail` 既定 / `continue` で `timeout` 出口）。期限なしの承認も run の期限が来れば取り消される。続けて待つ承認の期限の合計が run の期限以上なら保存時に警告する（`ir.approval_timeouts_exceed_run`）。エディタは承認を置くと `run_timeout_sec` を 14 日にする提案を出す。
- **出口**（§5.2）: 判断の状態ならその出口、判断の外の状態へ遷移した・行が消えたら `withdrawn`。停止時の rev（`suspend_rev`）以下の遷移では起きない（直前の自分の遷移で取り下げにしない）。待つ間に実行主体の委譲・テーブルの権限が外れたら、どの出口へも進めずに run を失敗させる（`actor_unavailable`・後続が委譲なしで中途半端に走らない・D-62・[engine.md](./engine.md) §9.6）。
- **停止時の検査**: 行の状態（`from_state`）から判断の行き先への遷移が FSM に無ければ `approval_not_decidable`（permanent）。担当者がいなければ `approval_no_assignee`。
- **出力**: `{ approval_id, decision: approve|reject|return|withdrawn|timeout, to, decided_by, decided_at, comment, record_id, rev }`（行の値は載せない）。
- **要求スコープ**: `notify.send`（人へのお知らせ・承認の依頼を含む・D-25）と `data.read`（行を読み、判断の遷移を実行主体の見え方で受け取る）。保存時の scope 補完が両方を足し、同意計画はテーブルの閲覧を提案する。
- **置き場所**: event トリガのワークフローに置くことを推奨する（interactive の run に置くと、後続も申請者本人の権限で走る）。
- 実行時（台帳・受付の突合・起床・期限・取消）は [engine.md](./engine.md) §9.6。

### 7.10 `workflow.reply`（結果を画面に返す）

```jsonc
{ "id": "reply_answer", "type": "workflow.reply", "label": "回答",
  "params": {
    "format": "markdown",                 // text | markdown | table | file_link
    "label": "回答",                       // 省略可・40 字まで
    "tone": "neutral",                    // neutral（既定）| success | warning | error
    "value": { "$from": "nodes.answer.output", "path": "/text" },
    "columns": [ { "label": "条項", "path": "/clause" } ],      // table のときだけ・20 列まで
    "file": { "id_path": "/file_id", "name_path": "/file_name" } // file_link のときだけ・省略は /file_id（無ければ /id）・/name（無ければ /file_name）
  } }
```

- **表示されるのは、interactive の run を起動した本人（ミニアプリの画面のボタン・会話の図のボタンを押した人）の画面だけ**。schedule・event の run（実行主体は workflow プリンシパル）、`workflow.start` の子、ワークフロー画面から起動した run の結果はどこにも出ない（読取口が principal・principal_kind = user・org・trigger_kind = interactive で絞る・[engine.md](./engine.md) §11.6）。会話の結果も押した本人にだけ見せ、次の回答の履歴（モデル）には渡さない。
- pure で、能力を呼ばず secret も解決しない。要求スコープは無い（scope 天井の許可に含める）。
- **本体（`parent: null`）にだけ置ける**（map 領域の中は `ir.reply_in_map`・要素ごとの結果は map の集約出力を table で返す）。中間に置ける（「受け付けました」を返してから `control.wait`・`human.approval` へ進む）。分岐ごとに別の reply を置け、実行されなかった側は skipped で画面に出ない。
- **整形**（executor）: text・markdown は文字（数値・真偽は文字にする・null は空・オブジェクトと配列は `invalid`）。table は配列の各要素（オブジェクトか配列）が 1 行で、`columns` があれば各セルを `path` で引き、無ければ先頭 20 行のキーを最大 20 列（並びは名前順）。セル 500 字・100 行（`total_rows` に元の行数）・null は空欄・オブジェクトは短い JSON。file_link はオブジェクトか配列で 20 件まで、ID が UUID でなければ `invalid`、同じ ID は 1 件にまとめる（社内検索の同じファイルの複数の抜粋）、名前は 200 字（無ければ「ファイル」）。中身の無いオブジェクト（`{}`・エディタの「項目を並べる」で 0 行のとき・参照が `{}` に解けたとき）は空の配列と同じく 0 件にする（1 件目のファイルの ID が無いと失敗にしない・BUG-168）。1 節は 16KB まで（文字は境界で切り、行・件数は末尾から落とし、`truncated` を立てる）。解決できない参照は `expr_resolve_error`。
- **空の値**（BUG-168）: `value` が空（空の文字・空白だけ・null・中身の無い `$template`）なら、形によらず保存は通し（V7 のリテラルの型も照合しない）、対話の実行（ボタン・会話・ワークフローの画面からの起動）の前に止める（§8 の `ir.required_text_empty`・空の結果を返さない。文言は「入れるまで、ボタンや会話からは実行できません。」）。自動実行のオンと、自動実行（schedule / event）・子の実行（`workflow.start`）の起動では止めない（これらの run の結果はどの画面にも出ない〔[engine.md](./engine.md) §11.6〕ので、前から自動実行がオンの版や呼び出される側を止めない・BUG-51 との重なり）。表・ファイルへのリンクの空のリスト（`[]`）と、ファイルへのリンクの中身の無いオブジェクト（`{}`）は止めず、保存の注意（`ir.empty_values`）だけにする。
- **出力**（画面に出す節そのもの）: `{ format, label?, tone, truncated, text? | columns, rows, total_rows | files: [{ id, name }] }`。正本はこの step の出力で、別の場所へ写さない（実行履歴の保持期間に従う）。
- **結果の Markdown の中の操作を持つ図・表**（` ```ui `・` ```yui `・` ```data_grid `）は、読取のときにすべて ` ```text ` に書き換え、画面も操作の宛先を持たない文脈で描く（D-28）。結果からの再起動はできない。画像は読み込まずリンクにし、外部リンクにはホスト名を添える。
- 固定の文言（`label`・`columns[].label`・リテラル・`$template` の地の文）はアプリを使う人全員に見える。値はすべて押した本人の権限で読んだもの。
- **置き方の指針**: 成功の結果は副作用の最後に置く。業務上の失敗は、失敗し得るノードを `on_error: continue` にして `error` 出口の先に `tone: error` の reply を置く（run は処理済み失敗として成功になり、画面は「完了」と出す）。
- 失敗した run の理由（利用者向けの分類・参照番号）は reply ではなく読取口が step の code から作る（[engine.md](./engine.md) §11.6）。

### 7.11 `ai.decide`（AI で判定・決定モデル）

```jsonc
{ "id": "triage", "type": "ai.decide", "label": "問い合わせを判定",
  "params": {
    "model": "decide-haiku",                                  // 判定用のモデルの id（必須・リテラル）
    "state": { "$from": "nodes.create.output", "path": "/data" },  // 判定の材料（値式・信頼しないデータ）
    "questions": [                                            // リテラルだけ・1〜20 件・順序を保つ
      { "key": "category", "type": "choice", "instructions": "問い合わせの種類はどれか",
        "options": [ { "key": "billing", "label": "請求", "description": "請求書・返金についての問い合わせ" },
                     { "key": "bug", "label": "不具合" }, { "key": "howto", "label": "使い方" },
                     { "key": "other", "label": "その他" } ] },
      { "key": "priority", "type": "score", "instructions": "対応の緊急度", "levels": ["低", "中", "高", "緊急"] },
      { "key": "angry", "type": "boolean", "instructions": "顧客は強い不満を示しているか",
        "yes": "苦情・強い不満・解約の示唆がある", "no": "通常の問い合わせ" } ],
    "route": { "question": "category", "by": "confidence", "min_confidence": 0.8 }   // 省略可
  },
  "on_error": "continue" }
```

- **何をするか**: 判定の材料について、質問ごとに決められた値域で答え、確からしさ（0〜1）を付けて返す。文章は生成しない。呼ぶのは llm-gateway の判定のチョークポイント `LlmGateway::decide`（[design.md](../design.md) §4.5）で、組織の許可（判定用のモデルと、LLM に判定させるなら元にする chat モデルの両方）・利用上限・会計はその中で行う。
- **`model`**: 判定用のモデル（カタログの `modality = "decision"`）の id。判定には組織の既定が無いので必須で、リテラルだけ（`$from` は型で弾く）。保存時に組織が許可した判定用のモデル（元にする chat モデルも許可されているもの）へ照合し（§8 V3）、実行時にもチョークポイントが照らし直す（無効なら既定へ戻さずに `model_unavailable` で失敗する）。組織がモデルの選択を禁じていても（`ir.model_choice_disabled`）、判定のモデルには掛けない。
- **`state`**: 値式。実行時に文字・オブジェクト・配列のどれかに解決され、JSON にして 32 KiB まで（超えたら `decision_state_too_large`・permanent）。**信頼しないデータ**として扱う（決定器は材料の中の指示に従わない。LLM に判定させる決定器は材料を封筒に包み、閉じタグを中和する・PIT-56）。リテラルで書くなら文字・オブジェクト・配列（V1）。
- **`questions`**（リテラルだけ・V1 で検査する）:
  - `key` は `^[a-z][a-z0-9_]{0,63}$` でノードの中で一意。`instructions`（画面の「AI に尋ねること」）は 2000 字まで。空のまま（空白だけを含む）でも保存はできるが、入れるまで実行と自動実行のオンを止める（§8 の保存を止めない警告・`ir.required_text_empty`・path は `/params/questions/<i>/instructions`・D-108）。実行時に空なら決定器を呼ばずに `bad_params` で失敗する。`type` は `choice` / `score` / `boolean`。
  - `choice`: `options`（2〜100 件）。選択肢の `key` は同じ形で一意、予約名（`out`・`uncertain`・`error`・`default`・`timeout`・`true`・`false`）は不可（出口の名前と取り違えないため）。
    - `label`（選択肢の名前・省略可・D-87）は利用者に見せる名前で、答えの `label` になる（テーブルの選択肢の列に書き込むなら、列の選択肢と同じ文字にする）。100 字以内で、質問の中で重ならない（空白だけは書いていないものとして扱う）。エディターは出口を名前で出す。
    - `description`（AI への説明・省略可）は AI への手がかりで、500 字以内。決定器へは `label` とつないだ「名前: 説明」を渡す（片方だけならそれ）。つないだ長さが 500 字に収まること（名前と説明を合わせて 498 字まで）。
    - `label` を省略した定義は、`description`（それも無ければ `key`）を答えの `label` にし、決定器へ渡す文も `description` のままにする（名前を足す前の定義の結果・プロンプト・決定キャッシュのキーを変えない）。
  - `score`: `levels`（低い順・2〜10 件・一意・各 1〜100 字）。
  - `boolean`: `yes` / `no`（それぞれに当たる場合の説明・500 字以内・省略可）。
  - 種類に合わない項目（choice に `levels` など）は拒否する。
  - 質問をリテラルに限るのは、材料（信頼しないデータ）と質問（作者のコード）を分け、出口を保存時に静的に決めるため。
- **`route`**（省略可）: `{ question, by, min_confidence }`。`question` は choice か boolean の質問（score は振り分けない。後ろの `control.branch` で `/answers/<key>/level_index` を使う）。`by` は `value`（既定）か `confidence`。`min_confidence` は 0.5〜1.0（しきい値は版ごとに実測で決めるもので、既定を置かない・PIT-81）。空のまま（項目なし・null）でも保存はできるが、入れるまで実行と自動実行のオンを止める（§8 の保存を止めない警告・`ir.required_text_empty`・D-88）。実行時に空なら決定器を呼ばずに `bad_params` で失敗する。エディターは既定を入れず、欄の横の「0.80 から始める」で仮の値を入れられる（仮の値を選んだことが本人に分かる）。
- **出口**（§5.2）: `route` 無し＝`out`。`by: confidence`＝確からしさがしきい値**以上**なら `out`、届かなければ `uncertain`。`by: value`＝しきい値以上なら答えの選択肢のキー（boolean は `true` / `false`）、届かなければ `uncertain`。`out`・`default` は出さないので、値で振り分ける判定からそれらへ張ったエッジは保存できない（`ir.unknown_port`）。`on_error: continue` なら `error` が加わる。
- **較正されていない確からしさでも同じ規則で振り分ける**（D-52 の H4）。出力の `calibrated` が false の決定器（LLM の自己申告・得票率）は確からしさが高めに出がちなので、しきい値は版ごとに実測で決める（PIT-81）。
- **しきい値未満は `uncertain` へ出すだけ**で、承認待ちを知らない。人に関わる判断（経費・人事など）は `uncertain` を `human.approval`（§7.9）へつなぎ、AI の判定だけで承認・却下を決めない構成にする（D-7）。材料の書き方で誤った答えに誘導されることはある（値域の内側の誤誘導は防げない）。
- **出力**:

  ```jsonc
  { "answers": {
      "category": { "type": "choice", "value": "billing", "label": "請求", "confidence": 0.9,
                    "probabilities": { "billing": 0.9, "bug": 0.03, "howto": 0.04, "other": 0.03 } },
      "priority": { "type": "score", "value": 1.0, "level": "中", "level_index": 1, "confidence": 0.8,
                    "probabilities": [0.05, 0.8, 0.1, 0.05] },
      "angry":    { "type": "boolean", "value": false, "probability": 0.2, "confidence": 0.8 } },
    "model": "decide-haiku",
    "resolved_model": "claude-haiku-5-5+llm_decider/v1/k1/low",
    "provider": "llm_decider",                                   // 答えた決定器（llm_decider・typesafe・stub）
    "calibrated": false, "method": "verbalized", "samples": 1,
    "cached": false,                                             // 決定キャッシュの答えなら true
    "routed": { "question": "category", "port": "out" },        // route が無ければ null
    "raised_questions": [] }                                     // AI の自信を引き上げた質問のキー（無ければ空）
  ```

  `label` は選択肢の名前（`label`・無ければ説明、それも無ければキー・D-87）。score の `value` は確率で重み付けした位置（0〜段階の数-1）、`level_index` は選んだ段階。`resolved_model` は実際に答えた決定器の版で、版が変わったらしきい値を測り直す。`provider` は答えた決定器の名前（国外処理の TypeSafe 互換か、LLM に判定させたものかを項目で絞れる・ポートが名前を返さなければ null）。`method` は `native`（決定器が出す確率）・`verbalized`（LLM の自己申告）・`vote`（多数決の得票率）。`raised_questions` は、AI が自分の答えに一様（1/n・はい/いいえなら 0.5）より低い自信を申告し、確からしさを一様まで引き上げた質問のキー（質問の順・D-78 の (3)・BUG-124・D-114）。取り違えや迷いの合図で、作る人は実行履歴の詳細で「AI が自信の低い答えを返しました」と見る。引き上げた答えも同じ規則で振り分ける。入りうるのは `verbalized` のときだけで、ほかの決定器・多数決・この項目の無い決定キャッシュの古い答えでは空。
- **応答の突き合わせ**: エンジンはポートの応答を型付きで読み、質問と突き合わせる（答えの過不足・種類の取り違え・選択肢の外・確率のキーが選択肢と一致しない・確率や確からしさの範囲の外・引き上げた質問が質問に無いか重なるは `decision_invalid_response`・permanent）。チョークポイントも同じ規則で突き合わせ（多層防御）、合わない応答（決定モデルの API が設定と違う版で答えた・応答の形が崩れたものも）はポートが同じ `decision_invalid_response` に写す（送り直しても直らないので再試行しない。試すたびに `#f` で課金されない）。
- **失敗**: 判定用のモデルが未登録・判定用でない・組織で無効（元の chat モデルが無効なときも）は `model_unavailable`、判定の拒否は `llm_refused`、応答の途切れは `llm_output_truncated`、決められた形に合わない応答は `llm_output_invalid`（どれも permanent）。到達障害・利用上限は llm.invoke と同じ（再試行の分類・[engine.md](./engine.md) §7）。
- **監査**: 出口と、振り分けに使った質問の確からしさだけを残す（要求したモデル・`resolved_model`・`provider`・`method`・`samples`・`calibrated`・`cached`・質問のキー）。**判定の材料と全部の答えは載せない**（D-52 の H5。run の step の出力には残る）。
- **決定キャッシュ**（step 属性 `cache`・§4・D-52 の H6）: 書いたノードだけ、同じ答えを使い回す（既定は使わない）。
  - **キー**: `sha256(キーの版 ‖ 決定器の版 ‖ 正規化した質問 ‖ 正規化した判定材料)`。正規化はオブジェクトのキーを並べ替えた JSON（配列の順・質問と選択肢の並びは保つ。プロンプトの順になるため）。決定器の版は応答の `resolved_model` と同じもので、構築時にカタログから決まる（LLM に判定させる決定器は元の実 ID・プロンプトの版・回数・思考の深さ）。プロンプトの改訂・モデルの差し替え・回数の変更は別のキーになる。答えた版が構築時の版と違う応答（決定モデルの API が別の版で答えた等）は書かない。
  - **範囲**: テナント＋組織＋ワークフロー。組織全体で共有すると、他人が同じ文面を判定させたかを当たり外れで推測できる。別のワークフロー（同じ IR を別に保存したものも）・別の組織では当たらない。
  - **期限**: 書き込みから `ttl_sec`。読むときはノードの今の `ttl_sec` でも絞る（短くしたら古い答えは当たらない）。期限を過ぎた行は書き込みのたびにテナントの中で消す。DB（`llm_decision_cache`）に持つのは答え（使用量は 0 にしたもの）とキーで、判定の材料そのものは持たない。
  - **掃除**: 答えは run の step の出力と同じく、実行履歴の保持期間（`tenant.workflow_retention_days`・[engine.md](./engine.md) §12.2）を超えて残さない。ワークフローを消すと、その答えは同じ TX の中で消える（論理削除に掛けるトリガ）。期限切れの行・保持期間より前に書いた行（`ttl_sec` が保持期間より長くても）・ワークフローの無い行は、日次の実行履歴 GC がテナントごとに消す。保持期間が `ttl_sec` より短いテナントでは、保持期間が実際の上限になる。
  - **当たっても組織の許可を確かめる**: 判定用のモデルと元の chat モデルの両方（チョークポイント `LlmGateway::decide_cached`・[design.md](../design.md) §4.5）。管理者が止めたら当たりでも `model_unavailable` で失敗する。**当たりは会計しない**（決定器を呼ばないので費用が出ない・利用上限も確かめない）。失敗した判定は使い回さない。
  - 当たったかは出力と監査の `cached` に出る。キャッシュの読み書きの失敗は判定を止めない（外れとして決定器を呼ぶ・書けなければ使い回さないだけ）。
- **会計**: チョークポイントが記録する。使えた判定は step の冪等キー、使えなかった判定（失敗・突き合わせで弾いたもの）で使用量のあるものは `{step のキー}#f{uuid}`（[engine.md](./engine.md) §7.2）。LLM に判定させる決定器は、元の chat モデルの名前・プロバイダの枠で数える。
- **要求スコープ**: 無い（scope 天井の対象外。llm.invoke と同じ内部推論で、データへの到達は材料を読んだ前のノードの data / storage のスコープで縛られる・D-52 の H14）。
- **保存を止めない警告**: しきい値が 0.6 未満（`ir.decide_low_confidence`）、値で振り分ける選択肢が 10 件以上（`ir.decide_many_ports`・確からしさで分けて後ろの振り分けで `/answers/<key>/value` を使う方が読みやすい）、モデルの名前が `-latest` / `-preview` を含む（`ir.decide_unpinned_model`）。

### 7.12 同じレコードを探す列と足す数（`data.record.upsert`・`add`・`match_on`・GAP-16）

同じものを 2 件にしない書き込み（同じ品番・同じ回答 ID・同じ週の集計を、再実行や取り込み直しで増やさない）と、数の足し引き（在庫を
1 減らす・合計に足す）を、探してから書き分ける組み方（`data.query` → `control.branch` → `data.record.create` / `update`）なしに書く。
探してから書き分けると、同時に動いた 2 つの実行が両方とも「無い」と見て 2 件追加し、読んだ値に足して書くと、間にほかの実行が
足した分が消える。

```jsonc
{ "id": "restock", "type": "data.record.upsert",
  "params": {
    "table": "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1",
    "match_on": "sku",                                        // 同じレコードを探す列（重複を許さない列の参照名・リテラル）
    "data": { "sku": { "$from": "input", "path": "/sku" },    // 入れる値（探す列の値を含む）
              "name": { "$from": "input", "path": "/name" } },
    "add": { "stock": { "$from": "input", "path": "/qty" } }  // 足す数（省略可・負の数で減らす）
  } }
```

- **`data.record.upsert`**: 実行主体に見えるレコードのうち、`match_on` の列の値が `data` の同じ列の値と等しいもの（`data.query` と同じ
  比べ方）を探す。あれば `data` から探す列を除いた値で書き換え（1 件の更新と同じ入口・書き換えの権限）、なければ `data` で追加する
  （1 件の作成と同じ入口・追加の権限・追加だけの人も追加できる）。`add` の数の列は、書き換えならいまの値に、追加なら 0 に足す。
  出力は書いたレコード（`data.record.create` と同じ項目）と `created`（追加したら true）。
  - 同じ値で並行に追加しても 2 件にならないのは、探す列の一意（`data_unique`）に頼る。追加が `unique_violation` で断られたら、先に
    追加されたレコードを探し直して書き換えに回る。探し直しても見えなければ（見えない行に同じ値がある・ほかの重複を許さない列が
    重なる）、その `unique_violation` を返す（1 件の追加と同じ応答・存在について明かすことは増えない・data-platform §16.1）。
  - 書き換えは、読んだ rev を前提に書く（`expected_rev`）。間に書き換えられた・消えたら探し直す（40 回・
    10 秒まで。重なり続けたら `conflict`〔やり直せる〕）。
  - 探す列の値は文字か数（空白だけの文字・null・リストは `invalid`）。2 件以上当たったら（探す列が重複を許していない）どれも
    書かずに `invalid`。状態の列は書けない（状態を進めるで変える・FSM の不変条件）。
- **足す数（`add`）**: `data.record.update` と `data.record.upsert` に書ける `{ "<数の列>": 足す数 }`（ValueExpr・解決した値は数）。
  いまの値（値が無ければ 0）に 10 進で正確に足した値を書く（`data::add_numbers`・数式の和と同じ演算）。
  - いまの値は**書き込む主体の見え方**で読む（書き換えの入口に何も変えない更新を渡し、data がいまのレコードを書き手のマスクで返す。
    リビジョン・outbox・監査は作らない）。schedule / event の run で、読取と書込を委譲した人が別々でも、読取の委譲元に読めない
    列を 0 から足して上書きしない（[engine.md](./engine.md) §6.8）。書き換えの権限が無ければ、足す前に断る。
  - 読んだ rev を前提に書き、間にほかの書込があれば読み直して足し直す（上と同じ回数まで）。並行に足しても足した分は失われない。
    `expected_rev` を書いたときは読み直さず、その版でなければ `conflict` で失敗する。
  - 足した結果は列の検証（`NumericSpec` の `scale`・`min`・`max`）を通る。在庫を 0 未満にしないなら列の最小値を 0 にする
    （外れたら書かずに `invalid`。並行の出庫でも、検証は書く値そのものに掛かるので 0 未満にならない）。
  - 値の欄（`patch` / `data`）と同じ列・探す列には書けない（保存時に `ir.schema_violation`・実行時に `invalid`）。
    `data.record.update` は `add` に列があれば `patch` を省ける（`ir.required_text_empty` にしない）。
- **まとめて登録の `match_on`**: 書くと、`id` の無い行は行の `data` の `match_on` の列の値で実行主体に見えるレコードを探し
  （200 件ずつ `in` で問い合わせる）、当たった行はそのレコードの更新（`id` を書いた行と同じ・`rev` は書いたときだけ照らす）、
  当たらない行は追加になる。取り込み直しても同じ値の行は増えない。行の `data` に探す列の値が無い・文字か数でない・同じ値の行が 2 つ
  あると、どれも登録せずに `invalid`。照合は効果の予約の内側で行い（記録済みの step は探し直さない）、取込 ID は冪等キーから
  決まるので、やり直しても二重に取り込まない。結果に当てた行の数 `rows_matched` が加わる。一括取込なので editor が要り
  （追加だけの人は使えない）、データ変更トリガは発火しない（§6.2）。省略・空の文字は従来どおり（`id` の無い行はすべて追加）。
- **まとめて登録の行の形**（BUG-255）: `rows` は 1 行ずつ `{ "id"?: uuid, "rev"?: 数, "key"?: 文字, "data": { 列の参照名: 値 } }` の
  配列で、書く値は `data` の中に置く（5,000 行まで・`MAX_WORKFLOW_BULK_ROWS`）。`data` の無い平らな行（`csv.query` の `items` の
  `{列の名前: 値}` をそのまま渡したもの）は実行で `invalid` になり、文言は 1 行ずつ `{"data": {列の参照名: 値}}` に包むよう言う
  （`rows` は値式なので保存時には照らさない）。CSV の行をテーブルに書くときは、`control.map` の `items` に `csv.query` の `items` を
  書き、領域の中の `data.record.upsert`（か `data.record.create`）の `data` に列ごとの `{"$from": "each", "path": "/item/<CSV の列の
  名前>"}` を書く（SQL で列の名前を参照名にそろえたときは `/item` をそのまま書ける・要素は 1,000 件まで・§5.3）。それを超えるときは
  `script.run` で 1 行ずつ `{data: 行}` に包んでから `rows` に渡す（`script.run` の戻り値は 256 KB まで・[script.md](./script.md)
  §4.3。包んだ行が超えるときは、行を分けて包む `script.run` を 2 つ置き、それぞれの結果を `data.bulk_upsert` に渡す・BUG-264）。AI の `emit_workflow` の説明文はこの順で教える。行の形の正は params の型 `DataBulkRow` で、実行はこの型で行を読み、
  `emit_workflow` の説明文の params の形は、この型の宣言から `rows` に書く値の形（`rows の値 = Array<{ id?, rev?, key?, data }>`）を
  添える（`rows` は `ValueExpr` とだけ言うと行の形が伝わらない・GAP-131）。
- **保存時の照合**（§8 V4）: `match_on` はテーブルで重複を許さない列（`unique`）であること（`ir.match_not_unique`。重複を許す列だと
  同時に動いた 2 つの実行が両方とも追加するため）。`add` の列は数の列であること（`ir.add_not_number`）。読めない列・無い列は
  `ir.unknown_field`（承認と同じ文言・区別しない）。材料は保存者の見え方（`Catalog::tables`）で、テーブルを `$from` で決める
  ブロックは照合しない（実行時に data が断る）。エディタは、テーブルを `$from` で決めるとき（と、テーブルを読めないとき）は
  列を選べないので、`match_on` を選ぶ欄の代わりに列のキーを打つ欄を出す（必須の `match_on` を画面から入れられる）。`add` の値に `$template` は書けない（組み立てた値は文字になる。`ir.schema_violation`・§8 V1・BUG-245）。入力の数を引くときは、入力の項目で負の数を受け取って `$from` で渡すか、`script.run` で負の数にしてから出力を `$from` で渡す。
- **要るスコープ**: `data.write`（探すのも書き込みの一部・1 件の更新が rev を読むのと同じ）。委譲は書き込みと同じ editor。
- **冪等**: 1 回の実行で成功する書込は高々 1 回で、ほかの誤りは何も書かずに返るので、効果の予約は 1 回の書込と同じ扱い
  （[engine.md](./engine.md) §7.3）。回収窓の at-least-once（予約を取ったワーカーが落ちた後の再実行）では、足す数が二重に
  足されることがある（`data.record.create` が二重に追加されうるのと同じ）。

### 7.13 日付を計算（`transform.date`・GAP-15）

前日・先週・先月・月末・週の初めを、script を書かずに求める（APP-03 の週報・APP-08 の週次 KPI・APP-11 の延滞など）。値の式には
計算を足さない（§3）。

```jsonc
{ "id": "last_week", "type": "transform.date",
  "params": {
    "base": { "$from": "run", "path": "/scheduled_at" },   // 基準の日（省略可・省略すると同じ）
    "shift": { "amount": -1, "unit": "week" },             // ずらす（省略可・amount は ValueExpr の整数）
    "period": "week",                                      // 期間（省略は day）
    "week_starts_on": "monday",                            // 週の初め（省略は monday）
    "tz": "Asia/Tokyo"                                     // 時間帯（IANA の名前・省略は日本時間）
  } }
// 出力: { "date": "2026-11-02", "start": "2026-11-02", "end": "2026-11-08",
//         "start_at": "2026-11-02T00:00:00+09:00", "next_start_at": "2026-11-09T00:00:00+09:00",
//         "year": 2026, "month": 11, "day": 2, "weekday": 1 }
```

- **基準の日**（`base`）: 日付（`YYYY-MM-DD`）か時差つきの日時（RFC 3339）。時差つきの日時は `tz` の日付として読む。時差の無い日時・
  ほかの形は読まない（保存時はリテラルを `ir.schema_violation`、実行時は `invalid`）。省略すると実行情報の `scheduled_at`（§3.1）。
- **ずらす**（`shift`）: `unit` は `day`・`week`（7 日）・`month`・`year`。`amount` は -10,000〜10,000 の整数（`MAX_DATE_SHIFT`。
  小数点つきの整数〔2.0〕は読む）。月・年でずらした先にその日が無ければ、その月の末日にする（1 月 31 日の 1 か月後は 2 月 28 日か
  29 日、5 月 31 日の 1 か月後は 6 月 30 日）。
- **期間**（`period`）: ずらした日を含む `day`・`week`（`week_starts_on` の曜日から 7 日）・`month`・`year`。
- **出力**: `date`（ずらした日）・`start`・`end`（期間の最初と最後の日・`YYYY-MM-DD`。テーブルの日付の列は `start` 以上・`end` 以下で
  絞る）・`start_at`・`next_start_at`（期間の始まりと次の期間の始まりの日時・`tz` の時差つき。日時の列は `start_at` 以上・
  `next_start_at` より前で絞る）・`year`・`month`・`day`・`weekday`（`date` の曜日・1＝月曜〜7＝日曜）。出力の形の正はカタログ。
- **時間帯と夏時間**: 計算は `tz` の暦の日付で行い、時刻を足し引きしない（夏時間で 23・25 時間の日をまたいでも日付がずれない）。
  `start_at`・`next_start_at` はその日の最初の時刻（ふつうは 0 時。0 時が無い日はその日の最初にある時刻、0 時が 2 回ある日は早い方）。
  組織の時間帯の設定はまだ無いので既定は日本時間（実行情報と同じ）。
- 日付は 1〜9999 年に収める（外れたら `invalid`）。能力を呼ばない pure なブロックで、スコープは要らない（scope 天井の対象外）。

---

## 8. 保存時検証パイプライン（確定・順序固定）

AI 編集も dnd 保存も**同一エンドポイント・同一パイプライン**を通る。ステージは固定順で走る。

| # | ステージ | 内容 | エラーコード例 |
|---|---|---|---|
| V1 | スキーマ | JSON Schema（deny-unknown・型・必須）・step 属性 `cache` を付けられる種と期限の範囲 | `ir.schema_violation`・`ir.cache_unsupported` |
| V2 | グラフ | id 一意・エッジ参照存在・ポート存在・DAG・入エッジ制約・領域閉包・出口一意・到達可能性 | `ir.graph_cycle` 等 |
| V3 | 語彙照合 | ノード type・declared_scopes・ツール名・モデル名を codegen 語彙と登録カタログ（モデルは組織が許可したもの・本人専用を除く）の閉じた集合へ照合。モデルの種類（文章を生成する chat・判定用の decision）。イベント source とトリガ・wait(event) の scope の形（§6.2・§7.5）。agent.invoke の道具と web.fetch の宛先（§7.4） | `ir.unknown_scope`・`ir.unknown_model`・`ir.model_choice_disabled`・`ir.model_modality_mismatch`・`ir.model_unavailable_for_user`・`ir.unknown_event_source`・`ir.bad_event_scope`・`ir.unknown_tool`・`ir.missing_egress`・`ir.bad_host`・`ir.host_outside_ceiling` |
| V4 | 参照存在 | skill（レジストリ＋インストール済み）・secret（存在＋宛先束縛がノード URL ホストを許容）・テーブル/フォルダ/ファイル id | `ir.unknown_skill`・`ir.unknown_table`・`ir.unknown_folder`・`ir.unknown_file`・`ir.field_not_indexed`・`ir.field_not_usable`・`ir.status_not_writable` |
| V5 | データフロー | `$from` の祖先性（map 領域の中は囲む map ノードの祖先も）・default 要否・条件木の型整合（静的に分かる範囲）・regex コンパイル | `ir.bad_ref` |
| V6 | script コンパイル | inline script の swc パース・禁止構文（[script.md](./script.md) §3。artifact 参照は version 固定なので存在のみ） | `ir.script_syntax` |
| V7 | 上限 | ノード ≤ 200・エッジ ≤ 500・IR ≤ 1MB・条件木深さ ≤ 5・テンプレ var 数 ≤ 50・承認の期限 < `run_timeout_sec`・`workflow.reply` ≤ 10・agent.invoke の手順・時間・トークン・依頼の大きさ（§7.4）・ノードの `timeout_sec`（1〜ノード種の上限・agent.invoke は `max_duration_sec` 以上・§4） | `ir.limit_exceeded` |

各ステージの詳細:

- **V1 スキーマ**: トップレベルからノード params まで、JSON Schema で型・必須・deny-unknown を検証する。未知フィールドは全階層で拒否する（§2）。誤りは最初の 1 件で止めず、位置つきでまとめて返す（GAP-35）: 型どおりに読めない IR は、全体・ブロック・線・きっかけを分けて読み、誤りの場所を外して読み直すことを繰り返して集める（全体は `/<項目>`、ブロックは `node_id` とブロックの中の位置〔`/params/...`・`/retry/...`〕、線は `edge` と `/edges/<i>/...`、きっかけは `/triggers/<i>/...`。項目が無い誤りは入れ物の位置で、項目の名前は文言が言う。1 つの部分につき 10 件・全体で 50 件まで。全体の件数か、serde を通す回数の合計〔2,000 回〕に達したら集めるのを止める。ブロック・線の数と IR の大きさが V7 の上限を超える読めない IR は、部分ごとに読まずに V7 と同じ `ir.limit_exceeded` だけを返す）。同じ回に、読めたブロックの設定（V1b）と入力項目の形（§2）の誤りも返す。ブロックの設定の誤りも項目ごとに返し、型の誤りがある間は項目どうしの検査（kind と供給する項目の整合など）を掛けない。`input_schema` の項目の並び（`x-order`）の形もここで確かめる（§2・D-60）。step 属性 `cache` は付けられる種（ノードカタログの `step_cache`・いまは `ai.decide`）だけで、ほかの種は `ir.cache_unsupported`（path は `/cache`）、`ttl_sec` が範囲の外なら `ir.schema_violation`（path は `/cache/ttl_sec`・§4）。`csv.query` の `tables` は、リテラルで書いた名前の形（英小文字・数字・`_`、先頭は数字以外、63 文字まで）・`data` 以外・重ねないことと、個数（8 個まで・`MAX_EXTRA_TABLES`）を、実行で断る tabular と同じ判定（`tabular_protocol::tables` の `table_name_issue`・`too_many_tables`）で確かめる（`ir.schema_violation`・path は `/params/tables/<i>/name` か `/params/tables`・GAP-126）。名前・要素・`tables` を値式で書いたものと、空の名前（必須の欄の空として有効化と実行の前に止める）は確かめない。`data.record.update`・`data.record.upsert` の `add` に `$template` を書く（全体でも列ごとの値でも）と `ir.schema_violation`（組み立てた値はいつも文字になり、実行で必ず「数を入れてください」で失敗する・§7.12・BUG-245）。
- **V2 グラフ**: `id` 一意、`parent` の連鎖が自分へ戻らないこと（自分を親にした map・互いを親にした map は `ir.bad_region`）、エッジの `from` / `to` が実在ノード、`from_port` が §5.2 のポートカタログに存在（出口はカタログの `port_rule` から導く。検証 API はノードごとの出口を `node_ports` で返す）、DAG（閉路なし）、join 以外の入エッジ ≤ 1、map 領域の閉包（領域跨ぎエッジなし）・出口ちょうど 1（中にノードのある領域だけ。空の領域は保存を止めない注意 `ir.empty_region`・§5.3）、全ノードのエントリからの到達可能性を検証する。孤立ノードは保存を止めない注意（`ir.unconnected_node`・§5.1）。`workflow.reply` は map 領域に置けない（`ir.reply_in_map`・§7.10）。
- **V3 語彙照合**: ノード `type`・`declared_scopes`・ツール名（`allowed_tools`）・モデル名を codegen が生成した閉じた語彙と、テナントの登録カタログ（モデルは組織が許可したモデル・本人専用を除く）へ照合する。**ハルシネーション境界**であり、実在しない語彙を参照する IR（特に AI 生成）を保存前に拒否する。`llm.invoke` の `model` は省略可で、省略（空文字を含む）は照合しない（実行時に組織の既定で解決する・§7.3）。組織がモデルの選択を禁じていれば、既定と異なるモデルを `ir.model_choice_disabled` で拒否する。
- **V3 のモデルの種類**: カタログのモデルは文章を生成する chat と判定用の decision に分かれる（[design.md](../design.md) §4.5）。`llm.invoke`・`agent.invoke` の `model` は組織が許可した chat モデル、`ai.decide` の `model` は組織が許可した判定用のモデル（LLM に判定させるなら元にする chat モデルも許可されているもの）へ照合する。種類を取り違えたもの（`llm.invoke` に判定用のモデル・`ai.decide` に chat モデル）は `ir.model_modality_mismatch`（path は `/params/model`・使えるモデルを添える）、どちらにも無いものは `ir.unknown_model`。判定用のモデルが 0 件の組織では `ai.decide` をすべて拒否する。照合の材料（組織の許可リスト）は保存 API・検証 API・AI の `emit_workflow` が同じ関数（`build_catalog_from`）で組み、種類の振り分けもその中だけで行う（保存と AI 編集で結果を変えない）。
- **V3 の保存する人の制限**（D-169・D-185）: モデルの照合の材料（`build_catalog_from`）は保存する人の文脈で組み、その人の利用者ごとのプロバイダの制限を通す（llm-gateway の `settings_for_workflow_editor`・判定はチャットの一覧と同じ `provider_allowed_for`）。組織では使えるがその人に許されないプロバイダのモデル（判定用のモデルは元にする chat モデルが許されないものも）を `llm.invoke`・`agent.invoke`・`ai.decide` に書くと `ir.model_unavailable_for_user`（path は `/params/model`・その人が書けるモデルを添える）。`llm.invoke`・`agent.invoke` で `model` を省いたときは、実行時と同じ解決（`resolve_workflow_choice`）で動くモデルがその人に使えなければ同じコードで拒否する。既定・Auto の振り先・選択の禁止は実行時の設定のまま読む（チャットのように既定を Auto へ差し替えない）。保存 API・検証 API・AI の `emit_workflow` は同じ材料を使う。制限の無い人・人でない主体には何も足さない。
- **V4 参照存在**: skill（レジストリ＋インストール済み）・secret（存在＋宛先束縛がノードの `http.request` URL ホストを許容する）・テーブル/フォルダ id の実在を検証する。`workflow.start` の `name`（リテラル）は、保存者に見える既存のワークフローであること（無い名前と見えない相手は同じ `ir.unknown_workflow`・同じ文言で、path は `/params/name`。存在を明かさない・BUG-107）。保存中の IR 自身の `name` は見えるものとして扱う（自分を呼ぶ新しいワークフローを最初の保存で止めない。互いに呼び合う 2 つは、片方を相手なしで保存してから相手を足す）。材料は保存 API・検証 API・AI の `emit_workflow` が IR の名前だけを保存者の権限で引く（`workflow_start_targets`）。空の名前は照合せず、必須の欄の空（`ir.required_text_empty`）として扱う。`human.approval` は、テーブルが保存者に見えて FSM で管理され、状態の列を保存者が読めること（見えないテーブル・FSM でないテーブル・状態の列が読めないテーブルは同じ文言 `ir.unknown_table`。状態の一覧は出さない）、判断の行き先が FSM の状態であること（`ir.unknown_state`）、`show_fields` と担当者の列がテーブルの列（担当者はユーザー・部署の列）であること、`show_fields` に状態の列が無いこと（`ir.bad_show_field`。状態の列は保存者が読めるときだけ材料に入るので、読めない列の手がかりにならない）を照合する。`data.transition` のテーブル（リテラル）と、`data.transition` のイベントトリガの `scope.table` も、保存者に見えて FSM で管理され、状態の列を保存者が読めること（FSM の無いテーブルでは、ノードは実行のたびに失敗し、トリガはいつまでも起きない。見えないテーブルと同じ `ir.unknown_table` で、path はノードなら `/params/table`、トリガなら `/triggers/<i>/scope/table`。テーブルを `$from` で決めるノードは照合しない・BUG-105）。材料は保存 API（検証 API・AI の `emit_workflow` も同じ）が**保存者の見え方**（`DataStore::get_table_for_viewer`・テーブルの owner でなければ読める列だけ）で引く。読めない列は無い列と同じ `ir.unknown_field`（code・path・message が同じ。文言も「このテーブルに無いか、読めない列です」とどちらとも言わない）になり、種別の違い（`ir.bad_assignee`）も返さない（無い列と読めない列を区別させない・data-platform §16.1）。`data.record.upsert` と `data.bulk_upsert` の `match_on`（リテラルのテーブル）は、保存者に読めて重複を許さない列であること（`ir.match_not_unique`）、`data.record.update` と `data.record.upsert` の `add` の列は数の列であること（`ir.add_not_number`）。無い列・読めない列は同じ `ir.unknown_field`（§7.12・GAP-16）。 業務データのほかのブロック（`data.query`・`data.record.create`・`data.record.update`・`data.record.upsert`・`data.record.delete`・`data.bulk_upsert`）・データ変更のきっかけ（`data.record.created`・`data.record.updated`）・データを待つ `control.wait`（`kind: event`）のテーブルと、`storage.write`・`storage.list`・`csv.write` の `folder`・ファイル保存のきっかけとそれを待つ `control.wait` の `scope.folder` と、`storage.read`・`csv.query`・`csv.patch` の `file`（ドライブから選んだファイル・GAP-91）は、保存者に見えるもの（テーブルは閲覧か追加だけ、フォルダーとファイルは閲覧）であること（GAP-35）。見えないもの・無いもの（ごみ箱に入れたものを含む）・書き損じた id（UUID でない文字）は同じ `ir.unknown_table`・`ir.unknown_folder`・`ir.unknown_file`（code・path・message が同じ。文言は「選んだテーブルが見つかりません。削除されたか、見る権限がありません。」）で止める。照合するのはリテラルの id だけで、参照で決めるもの・空の欄は照合しない。きっかけと待つブロックの scope の id の形の誤りは V3 の `ir.bad_event_scope` が言う（二重に言わない）。材料は保存 API・検証 API・AI の `emit_workflow` が IR の名指しした id だけを保存者の権限でまとめて引く（`table_labels`・`node_labels`〔フォルダーとファイルは種類つきで一緒に引く〕。何を引くかは照合と同じ位置の表〔`referenced_tables`・`referenced_folders`・`referenced_files`〕が決める。引く口があれば UUID の名指しが無くても照らすので、UUID でない書き損じだけの IR も止まる）。保存のときだけの照合で、保存済みの版の実行は変えない（実行は実行主体の権限で断る）。 `data.query`（リテラルのテーブル）の絞り込み（`filter` の `all`・`any`・`not` の中の `cmp.field`）・並べ替え（`sort`）・まとめる列（`group_by`）・集計の列（`aggregate`・`having`・`group_sort` の指標の `field`）は、実行時の束縛と同じ data の判定（`data::check_field_use`）で照らす。索引の無い列（`indexed` でも `unique` でもない列。部分一致〔`contains`・`ends_with`〕は `searchable` でない列）は `ir.field_not_indexed`、その位置に使えない種類の列（逆向きのリンク・複数の値やリンクを持つ列の並べ替えと大小比較・システムの列のまとめ・数でない列の集計・文字でない列の部分一致など）は `ir.field_not_usable`、無い列と読めない列は同じ `ir.unknown_field`（同じ文言）。path は列を書いた位置（例 `/params/query/filter/all/1/cmp/field`・`/params/query/sort/0/field`・`/params/query/group_by/0`・`/params/query/aggregate/0/field`。前の版の書き方〔`sort: {field}` など〕の IR はその項目の位置）。システムの列（`_owner`・`_created_at`・`_updated_at`）と作成者の列は、絞り込み・並べ替えでは索引のある列として通る（まとめる列には使えない）。件数（`count`）の指標の `field` は、実行で読まない（SQL の `COUNT(列)` もこの形になる）ので照らさない。リンクの条件（`link`）と、まとめる列・集計の列の結合先の列（`別名.参照名`）は、つないだ先の定義が要るので照らさない（並べ替えと絞り込みでは結合先の列を使えないので、実行と同じく止める）。誤りの文言は、表示名のある列を「表示名」（参照名）で言う（エディタは表示名で列を見せ、AI は参照名で直す）。ワークフローの経路では索引を自動で作らない（data-platform §10）ので、保存で止める（BUG-244）。エディタの「探す条件と並べ替え」も、この照合で通る列（索引のある列・重複を許さない列・システムの列・作成者の列。逆向きのリンクと索引の無い計算の列は除く）だけを候補に出し（判定は `web/src/lib/data-view.ts` の `isWorkflowQueryColumn`。グリッドの自動昇格を前提にした候補とは分ける）、部分一致で検索できる（`searchable`）文字・複数選択の列は、索引が無くても探す条件の「含む」「で終わる」にだけ出し（`isWorkflowFilterUse`）、隠した列があれば、使える列の決まりと索引の付け方を欄の下で言う。保存した後に索引を外すなどして、いまは通らない列を書いた定義は、エディタが開いたときの検証 API の応答で誤りを受け、欄の下と、その列の条件・並べ替えの行の下に出す（行は選んだ列の名前のまま出す）。状態の進め方のあるテーブル（リテラル）では、`data.record.update` の `patch`・`add` と `data.record.upsert` の `data`（同じレコードを探す列を除く）・`add` に状態の列を書くと、また `data.record.create`（と、探す列が状態の列の `data.record.upsert`）の `data` の状態の列に最初の状態でない値をリテラルで書くと `ir.status_not_writable`（path は `/params/<欄>/<状態の列>`）。判定は data の 1 件の更新・作成と同じ関数（`data::reject_status_write`・`data::FsmInitial::apply`）。`data.record.upsert` の `data` の状態の列は、最初の状態でも止める（同じレコードがあって書き換えるときは必ず断られ、追加のときは書かなくても最初の状態になる）。値を参照で決めるもの・値の欄の全体を参照で決めるものは照らさない。状態の列が保存者に読めなければ状態の列として扱わない（承認と同じ・読めない列の手がかりにしない・BUG-251）。材料はどちらも保存 API・検証 API・AI の `emit_workflow` が同じ `fsm_tables` で引く（保存者の見え方の定義と、レコードを追加するテーブルの作成時の状態〔テーブルにピンされた状態の進め方の最初の状態〕）。
- **V3 の承認**: `human.approval` は `data.read` と `notify.send` の宣言が要る（保存時の scope 補完が足す）。
- **V3 の AI エージェント**: `allowed_tools` の未知の名前は V1（選択肢にない値）、予約の道具は `ir.unknown_tool`、要求スコープの宣言漏れは `ir.missing_scope`（path は `/params/allowed_tools/<i>`・保存時の scope 補完が足す）。`model` は `llm.invoke` と同じ照合（path は `/params/model`）。web.fetch を選んだときだけ、`egress_allowlist` を照合する（空は `ir.missing_egress`、形は `ir.bad_host`、管理者の天井の外は `ir.host_outside_ceiling`・path は `/params/egress_allowlist/<i>`）。
- **V5 の AI エージェント**: 依頼（`instruction`）をリテラルで書くなら文字（`ir.expr_type_error`・参照と差し込みは実行時に文字かを確かめる）。
- **V5 データフロー**: `$from` の `nodes.<id>` が祖先か（map 領域の中のノードは、囲む map ノード〔ネストなら外側の map ノードも〕の祖先も祖先に数える。map ノード自身は数えない・§5.3・D-123）、全パスで先行しない場合に `default` があるか、条件木の左右の型が静的に整合するか、`matches` の regex がコンパイルできるかを検証する（§3）。`control.wait` の `filter` では、届いたイベントを指す `event`（別名 `trigger` / `payload`）も書ける（§7.5）。
- **保存を止めない警告**（V3・イベント）: 同じテーブルの `data.record.updated` と `data.transition`（`ir.overlapping_data_triggers`）、wait(event) の filter の別名（`ir.wait_filter_alias`）、続けて待つ承認の期限の合計が実行の期限以上（`ir.approval_timeouts_exceed_run`・§7.9）。
- **V6 script コンパイル**: inline script を swc でパースし、禁止構文を弾く（[script.md](./script.md) §3）。artifact 参照は version 固定なので存在確認のみ。
- **V7 上限**: サイズ・数量の上限を検証する（すべて初期値・設定で調整可）。ブロック・線の数と IR の大きさは、読める IR でも読めない IR でもほかの段より先に確かめ、超えたら `ir.limit_exceeded` だけを返す（大きな IR のブロックの設定や入力項目の `pattern` を読み切らない・D-147・BUG-222）。`workflow.reply` の `value` がリテラルなら形と合うこと（text・markdown は文字・table は配列・file_link はオブジェクトか配列。`ir.reply_type`・path は `/params/value`）。
- **保存を止めない警告**（`workflow.reply`）: interactive のトリガが無い IR の reply は画面に出ない（`ir.reply_without_interactive`）。
- **保存を止めない警告**（中身の無い値）: 値を書く欄が中身の無いオブジェクト・配列（`data.record.create` の `data: {}`・`data.record.update` の `patch: {}`〔`add` に列があるときは除く〕・`data.bulk_upsert` の `rows: []`・`csv.patch` の `ops: []`・表かファイルへのリンクの `workflow.reply` の `value: []` とファイルへのリンクの `value: {}`・BUG-168）なら `ir.empty_values`（path は `/params/<項目>`）。保存・有効化・実行のどれも止めない。状態の進め方のあるテーブルに最初の状態だけのレコードを作る `data: {}` は正しい使い方で、前から保存されている定義にもある（子の実行〔`workflow.start`〕・ミニアプリが版を固定して起こす実行も同じ起動の道を通るので、止めると動かなくなる）。文言は、そのままだと何が起きるかを言う（例: 「レコードの値」に列がありません。このままでは、どの列にも値を入れずにレコードを追加します。）。
- **保存を止めない警告**（中の無い map）: 中にノードの無い `control.map`（`ir.empty_region`・node は map・path なし・D-85）。書きかけでも保存はできるが、有効化と実行の前に止める（§9）。文言は map のブロック名を名指しし、どの場面でも通る形（「{ブロック名}」の中にブロックがありません。1 件ごとに行う処理を中に置くまで、実行も自動実行のオンもできません。）にする。
- **保存を止めない警告**（`ai.decide`）: しきい値が低い（`ir.decide_low_confidence`）・値で振り分ける出口が多い（`ir.decide_many_ports`）・版の固定されていないモデル名（`ir.decide_unpinned_model`）（§7.11）。
- **保存を止めない警告**（テンプレート）: vars のキーを `{{key}}` と書いた（`ir.template_escaped_var`）・閉じの無い `{`（`ir.template_unclosed_brace`・§3.2・D-37）。
- **保存を止めない警告**（入力の参照）: ブロックの設定の `$from: input` の path が、入力項目（`input_schema`）に無い項目を指す（`ir.unknown_input`・path は参照の位置・GAP-35）。手動で実行するときのダイアログに欄が出ないので、値が入らないとブロックは失敗する（既定値があれば既定値を使う）。入力項目が項目（`properties`）を決めていない・無い項目も受けると書いた入れ物（`additionalProperties` が true か形）は言わない。保存・有効化・実行のどれも止めない（API や画面のボタンが名前で送る使い方があり得る）。
- **保存を止めない警告**（必須の欄）: `llm.invoke` の `prompt`・`agent.invoke` の `instruction`・`rag.search` の `query`・`csv.query` の `sql`・`workflow.start` の `name`、置いたばかりのブロックの選ぶ欄と文字の欄（`control.branch` の条件の調べる値 `cmp.left`・`control.switch` の `value`・`control.map` の `items`・`ai.decide` の `model` と `state`・`storage.write` と `csv.write` の `name` と `content`・`http.request` の `url`・D-83）と、`ai.decide` の質問ごとの `instructions`（D-108）と、出口を分ける `ai.decide` のしきい値 `route.min_confidence`（既定を置かない・項目なしと null・D-88）と、`skill.invoke` の `skill` と、業務データ・ファイルを選ぶブロックの値の欄（`data.*` の `table`・`data.record.create` の `data`・`data.record.update` / `data.record.delete` / `data.transition` の `record`・`data.record.update` の `patch`〔`add` に列があるときは除く・GAP-16〕・`data.record.upsert` の `match_on` と `data`・`data.transition` の `to`・`data.bulk_upsert` の `rows`・`storage.read` と `csv.query` と `csv.patch` の `file`・`csv.patch` の `base_rev` と `ops`・BUG-151）と、置いたばかりの `human.approval` の `title`・`record.table`・`assignees`（0 件・値のある担当者がいない・担当者の `field` が空）・使う判断の `to`（D-129・§7.9）と、`workflow.reply` の `value`（BUG-168・§7.10）が空（空文字・空白だけ・null・中身の無い `$template` を含む。参照は実行時まで分からないので対象外。業務データ・ファイルを選ぶブロックの値の欄は、項目が無いのも空で、V1 は項目の欠落で止めない。値を書く欄〔`data`・`patch`・`rows`・`ops`〕の中身の無いオブジェクト・配列は空と数えない〔次の `ir.empty_values`〕）なら `ir.required_text_empty`（path は `/params/<項目>`、調べる値は条件の中の位置 `/params/condition/…/cmp/left`、判定の問いは `/params/questions/<i>/instructions`、しきい値は `/params/route/min_confidence`、承認は `/params/record/table`・`/params/assignees`・`/params/assignees/<i>/field`・`/params/<approve|reject|return>/to`）。書きかけでも保存はできるが、有効化と実行の前に止める（§9・D-43）。エディタは置いたばかりのこれらの欄を `""` で置く（入力全体を黙って入れない）。`ai.decide` の `model` と `http.request` の `url` は、空なら V1 の照合（モデルの必須・URL の形）もしない（値を入れた後の保存で照合する）。`skill.invoke` の空の `skill` も V4 の照合（形・インストール済み）をしない。`human.approval` の空の欄も V1（字数・件数・ID の形）と V4（テーブル・状態・列）の照合をしない。文言は欄の見出しを名指しし、どの場面でも通る形（「{見出し}」が空です。入れるまで（選ぶ欄は選ぶまで）、実行も自動実行のオンもできません。）にする。

**エラー形式**: 最初の 1 件で止めず、全件を配列で返す（dnd がノード上に表示するため）。

```jsonc
{ "errors": [
    { "code": "ir.bad_ref", "node_id": "notify_manager", "path": "/params/message/vars/amount",
      "message": "$from nodes.compute は notify_manager の祖先ではなく default もない" },
    { "code": "ir.unknown_scope", "node_id": "post_slack", "path": "/declared_scopes/2",
      "message": "http.egres は未知のスコープ（http.egress の誤り？）" }
] }
```

- `edge` に紐づくエラーは `node_id` の代わりに `edge`（`{ "from", "from_port", "to" }`）を持つ。

**保存時検証と実行時検証の関係**: V3 / V4 は「保存時 OK でも実行時に消えている」ことがある（skill アンインストール・モデルカタログ変更等）。したがって**実行時にも再検証し fail-closed する**（[engine.md](./engine.md) §6）。保存時検証は UX とハルシネーション境界を担い、実行時検証が最終防衛線である。

---

## 9. バージョニング・ライフサイクル

- **artifact version は不変**。run は開始時に `(workflow_id, version)` を**ピン**し、実行途中で新版に乗り換えない。
- **有効化（enable）は version 単位**: `workflow_registration` が `enabled_version` を指す（[engine.md](./engine.md) §2/§10）。新版を公開しても、enable を切り替えるまで旧版が動き続ける。
- **`ir_version`（スキーマ版）は整数単調増加**。エンジンは N と N-1 を読める（それ以前は保存時マイグレーション必須）。フィールド追加であっても deny-unknown のため `ir_version` を上げる（安全側に倒す・頻度は低い想定）。
  - **ノード種の追加（予約語彙の解禁・新しい variant）は `ir_version` を上げない**。`Node.type` は文字列として読み V3 で語彙と照合するので、serde の形が変わらない（前例: `csv.*`・`data.*`・`human.approval`・`workflow.reply`）。上げるのはエンベロープや既存の構造体（トリガ・エッジ・ポリシー）へのフィールドの追加。
  - **`input_schema` の注記の追加（`x-order`・D-60）も `ir_version` を上げない**。`input_schema` は JSON Schema の値で、エンベロープの形は変わらない。旧いエンジンは知らない語を持つスキーマの起動を断る（fail-closed）。
  - **既存のノードの params への任意の項目の追加も `ir_version` を上げない**。旧い IR はそのまま読め、旧いエンジンは新しい項目を持つ IR を params の読み取りで拒否する（fail-closed・前例: `agent.invoke` の `allowed_tools`・`context`・`model`・`max_steps`・`max_tokens`）。
  - **旧いエンジンは、自分の知らない新しい `ir_version` の IR にピンした run を失敗させない**（ローリングデプロイの途中）。待ち行列の昇格では、そのワークフローだけを見送って新しい版のエンジンに任せる（[engine.md](./engine.md) §8.3・D-65）。知っている版のまま読めない IR だけを失敗にする。
- **削除は論理削除のみ**（実行履歴の run が version を参照し続けるため）。
- **scope 拡大時の再同意**: 新 version の `declared_scopes` が旧版より広がった場合、有効化済みワークフローは再同意が済むまで新版へ切替不可（§2・fail-closed）。縮小のみなら差分同意なしで切替可能（同意フローの UI 契約は [engine.md](./engine.md) §10）。
- **有効化と実行の前の確認**（保存は止めない・D-40・D-43）: 次の版は有効化できない（enable は委譲を書く前に 409 `not_ready` で全件の理由を返し、同意計画も同じ理由を `blockers` で返す）。
  - 必須の文字の欄が空（§8 の `ir.required_text_empty`）。対話の起動（API・会話の図・ミニアプリ・`workflow.start`）も run を作らずに断る（400 `not_ready`）。`workflow.reply` の返す値の空だけは、有効化と `workflow.start` の子の起動では止めず、ボタン・会話・画面からの起動の前にだけ止める（§7.10・BUG-168）。
  - 中にノードの無い `control.map`（§8 の `ir.empty_region`・§5.3・D-85）。対話の起動も同じく断る。
  - 値を書く欄の中身の無いオブジェクト・配列（§8 の `ir.empty_values`）は止めない（注意だけ）。
  - schedule / event のきっかけを持つ版で、`folder` を省略した（または null の）`storage.write`・`csv.write`（`ir.write_folder_required`）。省略はルートへの保存で、schedule / event の run はワークフロー主体で動くのでルートには書けず、フォルダの委譲でも救えない。対話だけの版は止めない（起動した本人のルートに書ける）。
  - 同じく、`folder` を省略した（または null の）`storage.list`（`ir.list_folder_required`・D-83）。ワークフロー主体はルートを一覧できない。対話だけの版は止めない。
  - 必須の文字の欄が空（`workflow.reply` の返す値の空を除く）・中にノードの無い `control.map` の版は、自動実行（schedule / event）の起動でも run を作らない。この確認より前に有効化した版や、確認する欄が後から増えた版でも、空のまま動かさない。見送りは再試行しない（委譲の不成立・定義を読めない見送りと同じ `DelegationInvalid` に理由の文を添える）。警告は版ごとに 1 回。きっかけは止めない（欄を埋めて自動実行をオンにし直せば直る・BUG-51）。保存先・一覧にするフォルダの省略は、この起動の確認では止めない（有効化の前の確認だけ）。

---

## 10. 通し実例

「経費申請の `data.transition` イベント → 金額分岐 → 上長通知 or 自動承認 → map で添付ファイル処理 → join → 完了通知」の完全な IR。triggers / nodes / edges / policies を全て含み、§8 の検証を通る形で示す。

```jsonc
{
  "ir_version": 1,
  "name": "expense-approval",
  "display_name": "経費承認フロー",
  "description": "経費申請の提出イベントを起点に、金額で承認経路を分岐し添付を処理する",
  "declared_scopes": ["data.read", "data.write", "storage.read", "storage.write", "notify.send"],

  "triggers": [
    { "id": "t_submitted", "type": "event", "source": "data.transition",
      "scope": { "table": "expense" },
      "filter": {
        "op": "eq", "left": { "$from": "trigger", "path": "/transition" }, "right": "submit"
      } }
  ],

  "nodes": [
    // 1) 金額で分岐（10 万円以上は上長承認、未満は自動承認）
    { "id": "check_amount", "type": "control.branch",
      "params": { "condition": {
        "op": "gte", "left": { "$from": "trigger", "path": "/record/amount" }, "right": 100000
      } } },

    // 2a) 高額: 上長へ通知（承認待ち）
    { "id": "notify_manager", "type": "notify.send",
      "params": {
        "to": { "$from": "trigger", "path": "/record/manager_id" },
        "message": { "$template": "{user} さんの経費申請（{amount} 円）が承認待ちです",
          "vars": {
            "user":   { "$from": "trigger", "path": "/record/applicant_name" },
            "amount": { "$from": "trigger", "path": "/record/amount" } } }
      },
      "retry": { "max_attempts": 3, "backoff": { "kind": "exponential", "base_sec": 5, "max_sec": 300, "jitter": true } } },

    // 2b) 低額: FSM 遷移 API で自動承認
    { "id": "auto_approve", "type": "data.transition",
      "params": {
        "table": "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1",   // 経費テーブルの id（§7.1・Phase 15.7 で名前ではなく UUID）
        "record": { "$from": "trigger", "path": "/record/id" },
        "to": "approved"                                   // 遷移名・遷移入力ではなく遷移先の status（§7.1）
      } },

    // 3) 両分岐の合流（どちらか一方だけ live・all で待って dead を吸収）
    { "id": "join_paths", "type": "control.join",
      "params": { "mode": "all" } },

    // 4) 添付ファイル一覧
    { "id": "list_attachments", "type": "storage.list",
      "params": { "folder": { "$from": "trigger", "path": "/record/attachment_folder" } } },

    // 5) map: 添付を要素ごとに並列処理（領域 = parent: map_attachments）
    { "id": "map_attachments", "type": "control.map",
      "params": {
        "items": { "$from": "nodes.list_attachments.output", "path": "/items" },
        "max_concurrency": 5,
        "on_item_error": "collect"
      } },

    //   領域入口: 添付を読む
    { "id": "read_file", "type": "storage.read", "parent": "map_attachments",
      "params": { "file": { "$from": "each", "path": "/item/file_id" } } },

    //   領域出口（出エッジ0・ちょうど1つ）: 監査用アーカイブフォルダへ複製を書き出す
    { "id": "archive_file", "type": "storage.write", "parent": "map_attachments",
      "params": {
        "folder": { "$from": "trigger", "path": "/record/archive_folder" },
        "name": { "$template": "archive-{i}", "vars": { "i": { "$from": "each", "path": "/index" } } },
        "content": { "$from": "nodes.read_file.output", "path": "/bytes" }
      } },

    // 6) 完了通知（申請者へ）
    { "id": "notify_done", "type": "notify.send",
      "params": {
        "to": { "$from": "trigger", "path": "/record/applicant_id" },
        "message": { "$template": "{user} さんの経費申請の処理が完了しました",
          "vars": { "user": { "$from": "trigger", "path": "/record/applicant_name" } } }
      },
      "retry": { "max_attempts": 3, "backoff": { "kind": "exponential", "base_sec": 5, "max_sec": 300, "jitter": true } } }
  ],

  "edges": [
    { "from": "check_amount", "from_port": "true",  "to": "notify_manager" },
    { "from": "check_amount", "from_port": "false", "to": "auto_approve" },
    { "from": "notify_manager", "to": "join_paths" },
    { "from": "auto_approve",   "to": "join_paths" },
    { "from": "join_paths",       "to": "list_attachments" },
    { "from": "list_attachments", "to": "map_attachments" },
    // 領域内エッジ（同一領域で閉じる）
    { "from": "read_file", "to": "archive_file" },
    // map の out（領域完了後）を完了通知へ
    { "from": "map_attachments", "to": "notify_done" }
  ],

  "policies": {
    "run_timeout_sec": 259200,
    "concurrency": { "max_parallel_runs": 10 },
    "on_trigger_overflow": "queue"
  }
}
```

この IR の検証上の要点（なぜ通るか）:

- **入エッジ制約（V2）**: `join_paths` のみ入エッジ 2 本（join なので許容）。他ノードは全て入エッジ ≤ 1。
- **map 領域閉包（V2）**: `read_file` と `archive_file` が `parent: map_attachments`。領域内エッジ `read_file → archive_file` は領域内で閉じる。入口 = `read_file`（入エッジ 0）、出口 = `archive_file`（出エッジ 0・ちょうど 1 つ）。
- **祖先性と default（V5）**: 各ノードのデータ参照は `trigger`（常に解決可能）・`each`（領域内のみ）・領域内祖先 `read_file` のみで、分岐の片側にしか存在しない祖先への参照がないため `default` は不要。`join_paths` の出力（dead 側が null）は誰も参照しない。
- **式言語を持たない帰結**: 「添付 N 件」のような**件数の算出は IR だけでは書けない**（`$from` は配列長を取れない）。件数が要るなら `map_attachments` の後に script ノードを 1 つ置き `input.items.length` を返す——これが「複雑な計算は script へ昇格」（§3）の実例である。
- **語彙・スコープ（V3）**: 使用ノードの要求スコープ（`notify.send`・`data.write`・`storage.read`・`storage.write`）は全て `declared_scopes` に含まれる。
- **skip 伝播**: `check_amount` の一方の分岐が dead になり、その下流（`notify_manager` か `auto_approve` のいずれか）が skipped になるが、`join_paths` が `all` で dead を吸収して `list_attachments` 以降へ前進する（実行時挙動は [engine.md](./engine.md) §4）。
