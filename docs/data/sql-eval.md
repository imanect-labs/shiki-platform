# SQL 前段面の LLM 生成成功率の評価

> Phase 15 Task 15.10 の受け入れ条件「SQL 入力の生成成功率」の記録。業務的に代表的な 50 問
> （単純フィルタ／複合条件／集約／ソート＋ページング／リンク経由 × 各 10 問）について、
> **IR 直書きの基準値を先に測り**、SQL 入力の改善幅を測った。設計の根拠は
> [data-platform.md §16.1「なぜ必要か」](../data-platform.md)。

## 結論

- **受け入れ条件の「+15 ポイント以上」は満たしていない。** 6 モデル × 50 問（300 試行）の初回正解率は、
  主条件（例 3 つあり）で IR 98.7% → SQL 99.7%（**+1.0 ポイント**）、
  例なしの条件で IR 98.0% → SQL 99.7%（**+1.7 ポイント**）だった。
  モデル別の改善幅の最大は +4 ポイント（例あり）／+6 ポイント（例なし）。
- 原因は**基準値が天井に近いこと**。今回の 6 モデルは、IR の文法・例・スキーマを渡すと IR を 94〜100% の
  確度で書けた。+15 ポイントの改善には IR の基準値が 85% 以下である必要があるが、どのモデル・どちらの
  条件でもそうならなかった。§16.1 の「IR は生成が安定しない」という前提は、この問題集合と今回の
  モデルでは観測できなかった。
- SQL 側で差が出たのは次の 3 点。
  - **IR の失敗はリンク経由（結合）に集中する**: 2 条件を合わせた初回の失敗は IR 10 件・SQL 2 件。
    IR の 10 件のうち 8 件がリンク経由で、中身は「結合先の列を `filter` に `c.city` と書く」
    「別名ではなくリンク列名で参照する」「集計なしの `group_by`」「JSON の括弧の誤り」。
    SQL の 2 件は `GROUP BY _id`（部分集合の制限）と、問いに無い条件を足した意味の誤りが 1 件ずつ。
  - **出力が半分の長さ**: 1 問あたりの出力は IR が平均 150〜180 文字、SQL が 75〜87 文字。
  - **構文エラーは SQL では 0 件**（IR は JSON の不正・IR の形の不正が 3 件）。
- 構造化した 422 を返して 1 回だけ書き直させると、どちらの面も 98〜100% まで戻った（下表「書き直し 1 回まで」）。

判定を変えるには、問題集合を難しくする（多段結合・入れ子の論理・HAVING の組み合わせを増やす）、
より小さいモデルで測る、または受け入れ条件を「正解率」以外（出力の長さ・自己修正の往復数）で
置き直すことが要る。いずれも受け入れ条件の変更にあたるので human の判断を待つ。

## 再現手順

```bash
SHIKI_SQL_EVAL_URL=<OpenAI 互換の chat/completions の URL> \
SHIKI_SQL_EVAL_MODELS=<モデル名,モデル名,…> \
SHIKI_SQL_EVAL_HEADER='<追加ヘッダ名>: <値>' \
SHIKI_SQL_EVAL_CA_BUNDLE=<プロキシの CA の PEM（必要なら）> \
SHIKI_SQL_EVAL_OUT=<結果 JSON の出力先ディレクトリ> \
STORAGE_TEST_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/shiki \
  cargo test -p shiki-data --test sql_eval_it llm -- --nocapture
# 例なしの条件は SHIKI_SQL_EVAL_NO_EXAMPLES=1 を足す
```

- 評価コード: [`crates/data/tests/sql_eval_it.rs`](../../crates/data/tests/sql_eval_it.rs)。
  `SHIKI_SQL_EVAL_URL` が無いときは LLM の評価を飛ばし、`gold_answers_are_valid`（正解の SQL が全問実行でき、
  結果が空でなく、正解の SQL を IR に変換して IR 直投入で流しても評価器が正解と判定する）だけが走る。
- 問題集合: [`crates/data/tests/sql_eval/questions.json`](../../crates/data/tests/sql_eval/questions.json)
  （問題文・カテゴリ・正解の SQL）。データ: [`seed.json`](../../crates/data/tests/sql_eval/seed.json) と評価コードの
  `seed()`（顧客 30・商品 24・注文 200・明細 400。金額・価格・日付は一意で、並べ替えに同値が出ない）。
- プロンプト: [`prompt_ir.md`](../../crates/data/tests/sql_eval/prompt_ir.md) と
  [`prompt_sql.md`](../../crates/data/tests/sql_eval/prompt_sql.md)。どちらも同じスキーマ説明
  （[`schema.md`](../../crates/data/tests/sql_eval/schema.md)）・同じ 3 問の例（意図は同じ・問題集合とは別の問い）・
  同じ「件数の上限」の説明を持ち、違うのは文法の説明だけ。
- 実施: 2026-10-23。OpenAI 互換のゲートウェイ経由、`temperature: 0`・`max_tokens: 8192`・6 並列。
  モデルは glm-5.3-flash・qwen3.8-flash・mimo-v2.6-flash・minimax-m3.1-flash・glm-5.1・kimi-k2.6。
  各モデルで **IR（基準値）を先に全問**、続けて SQL を全問測った。

## 判定の規則

- **正解**: 実データで実行した結果が、正解の SQL の結果と一致すること。
  - 正解の各列に、値の多重集合が等しい候補の列を割り当てられ、その割り当てで行の多重集合が一致する。
    **余分な列・列の順序・列名は問わない**（IR で `select` を省いて全列を返しても正解になる）。
  - 並べ替えの問題（D01〜D10）は行の順序まで比べる。E09（「重複なし」）は重複を除いた集合で比べる。
  - 値は数値なら小数 4 桁に丸めて比べる（`AVG` の表現の差を吸収する）。
  - `LIMIT`（IR は `limit`）があれば 1 ページ目だけ、無ければ全ページを読む（両面で同じ規則）。
  - IR は `{"table", "query"}` で包まずに最上位へ並べた出力も受ける（包み方は評価器の都合で、
    ツールの引数なら別の形になるため。基準値に有利な側へ倒した）。
- **失敗の分類**（初回の失敗で数える）:

| 分類 | SQL | IR |
|---|---|---|
| 構文エラー | `syntax_error` | JSON として読めない・IR の形に合わない（`DataQuery` へ読み込めない） |
| 部分集合外 | `unsupported`・`invalid_join`・`limit_exceeded` | 結合・集計の組み合わせの検証エラー（`DataError::Invalid` のうち下 2 つ以外） |
| 列・テーブル名誤り | `unknown_name` | 「スキーマに存在しません」・存在しないテーブル名 |
| 意味的に不正 | 実行できたが結果が違う・`invalid_query`（型や集計の誤り） | 実行できたが結果が違う・値の型の誤り |

- **書き直し 1 回まで**: 初回がエラーで失敗した問いにだけ、API が返すのと同じ本文
  （SQL は 422 の構造化理由、IR はエラーの文言）を返して 1 回書き直させた。「結果が違う」は本番では
  分からないので返さない。

## 結果 1: 主条件（例 3 つあり）

カテゴリ別の正解数は 単純フィルタ / 複合条件 / 集約 / ソート＋ページング / リンク経由 の順（各 10 問）。
出力長は 1 問あたりの平均文字数。

| モデル | 面 | 初回 | 書き直し 1 回まで | カテゴリ別 | 初回の失敗 | 出力長 |
|---|---|---:|---:|---|---|---:|
| glm-5.3-flash | IR | 50/50 (100%) | 50/50 (100%) | 10 / 10 / 10 / 10 / 10 | — | 169 |
| glm-5.3-flash | SQL | 50/50 (100%) | 50/50 (100%) | 10 / 10 / 10 / 10 / 10 | — | 76 |
| qwen3.8-flash | IR | 50/50 (100%) | 50/50 (100%) | 10 / 10 / 10 / 10 / 10 | — | 152 |
| qwen3.8-flash | SQL | 50/50 (100%) | 50/50 (100%) | 10 / 10 / 10 / 10 / 10 | — | 76 |
| mimo-v2.6-flash | IR | 48/50 (96%) | 49/50 (98%) | 10 / 10 / 10 / 10 / 8 | 意味的に不正 1・部分集合外 1 | 168 |
| mimo-v2.6-flash | SQL | 50/50 (100%) | 50/50 (100%) | 10 / 10 / 10 / 10 / 10 | — | 76 |
| minimax-m3.1-flash | IR | 49/50 (98%) | 50/50 (100%) | 10 / 10 / 10 / 10 / 9 | 部分集合外 1 | 150 |
| minimax-m3.1-flash | SQL | 50/50 (100%) | 50/50 (100%) | 10 / 10 / 10 / 10 / 10 | — | 75 |
| glm-5.1 | IR | 50/50 (100%) | 50/50 (100%) | 10 / 10 / 10 / 10 / 10 | — | 167 |
| glm-5.1 | SQL | 50/50 (100%) | 50/50 (100%) | 10 / 10 / 10 / 10 / 10 | — | 75 |
| kimi-k2.6 | IR | 49/50 (98%) | 50/50 (100%) | 10 / 10 / 10 / 10 / 9 | 列・テーブル名誤り 1 | 167 |
| kimi-k2.6 | SQL | 49/50 (98%) | 50/50 (100%) | 10 / 10 / 10 / 10 / 9 | 部分集合外 1（※） | 76 |
| **合計（300 試行）** | IR | **296 (98.7%)** | 299 (99.7%) | | 構文 0・部分集合外 2・名前 1・意味 1 | |
| **合計（300 試行）** | SQL | **299 (99.7%)** | 300 (100%) | | 構文 0・部分集合外 1・名前 0・意味 0 | |

※ kimi-k2.6 の SQL E09（`GROUP BY c._id, c.name`）は、計測時は `invalid_query` を返したので評価コードは
「意味的に不正」に数えた。`GROUP BY _id` は SQL として妥当で部分集合の制限にあたるため、計測後に拒否コードを
`unsupported` に直し、この表では「部分集合外」に分類し直した。

## 結果 2: 例なし（両面のプロンプトから「## 例」の節を同じように外した感度分析）

| モデル | 面 | 初回 | 書き直し 1 回まで | カテゴリ別 | 初回の失敗 | 出力長 |
|---|---|---:|---:|---|---|---:|
| glm-5.3-flash | IR | 50/50 (100%) | 50/50 (100%) | 10 / 10 / 10 / 10 / 10 | — | 176 |
| glm-5.3-flash | SQL | 50/50 (100%) | 50/50 (100%) | 10 / 10 / 10 / 10 / 10 | — | 83 |
| qwen3.8-flash | IR | 50/50 (100%) | 50/50 (100%) | 10 / 10 / 10 / 10 / 10 | — | 173 |
| qwen3.8-flash | SQL | 50/50 (100%) | 50/50 (100%) | 10 / 10 / 10 / 10 / 10 | — | 81 |
| mimo-v2.6-flash | IR | 49/50 (98%) | 50/50 (100%) | 10 / 10 / 10 / 10 / 9 | 部分集合外 1 | 180 |
| mimo-v2.6-flash | SQL | 49/50 (98%) | 49/50 (98%) | 10 / 10 / 9 / 10 / 10 | 意味的に不正 1 | 83 |
| minimax-m3.1-flash | IR | 47/50 (94%) | 49/50 (98%) | 10 / 10 / 9 / 10 / 8 | 構文エラー 2・部分集合外 1 | 154 |
| minimax-m3.1-flash | SQL | 50/50 (100%) | 50/50 (100%) | 10 / 10 / 10 / 10 / 10 | — | 81 |
| glm-5.1 | IR | 49/50 (98%) | 49/50 (98%) | 10 / 9 / 10 / 10 / 10 | 構文エラー 1 | 169 |
| glm-5.1 | SQL | 50/50 (100%) | 50/50 (100%) | 10 / 10 / 10 / 10 / 10 | — | 87 |
| kimi-k2.6 | IR | 49/50 (98%) | 50/50 (100%) | 10 / 10 / 10 / 10 / 9 | 列・テーブル名誤り 1 | 168 |
| kimi-k2.6 | SQL | 50/50 (100%) | 50/50 (100%) | 10 / 10 / 10 / 10 / 10 | — | 78 |
| **合計（300 試行）** | IR | **294 (98.0%)** | 298 (99.3%) | | 構文 3・部分集合外 2・名前 1・意味 0 | |
| **合計（300 試行）** | SQL | **299 (99.7%)** | 299 (99.7%) | | 構文 0・部分集合外 0・名前 0・意味 1 | |

## 失敗の全件（初回）

「書き直し」は ○ = 書き直しで正解・× = 書き直しても失敗・— = 結果の違いなので書き直させていない。

### 主条件（例あり）

| モデル | 面 | 問 | 分類 | 出力 | 理由 | 書き直し |
|---|---|---|---|---|---|---|
| mimo-v2.6-flash | IR | E03 | 部分集合外 | `{"table": "customers", "query": {"group_by": ["city"], "aggregate": [{"metric": "sum", "field": "orders.amount"}]}}` | 別名 'orders' は結合にありません（`joins` を書かずに逆向きのリンク列を別名として使った） | ○ |
| mimo-v2.6-flash | IR | E05 | 意味的に不正 | `{"table": "order_lines", "query": {"filter": {"link": {"field": "order_ref", "filter": {"cmp": {"field": "order_no", "op": "eq", "value": "O-0010"}}}}, "select": ["qty"]}}` | 商品名を返していない（半結合で絞っただけ） | — |
| minimax-m3.1-flash | IR | E09 | 部分集合外 | `{"table":"customers","query":{"filter":{"link":{"field":"orders","filter":{"cmp":{"field":"status","op":"eq","value":"キャンセル"}}}},"group_by":["name"],"select":["name"]}}` | group_by には aggregate の指定が必要です | ○ |
| kimi-k2.6 | IR | E10 | 列・テーブル名誤り | `{"table": "order_lines", "query": {"filter": {"cmp": {"field": "c.city", "op": "eq", "value": "大阪"}}, "joins": [{"alias": "o", "field": "order_ref"}, {"alias": "c", "from": "o", "field": "customer"}, {"alias": "p", "field": "product"}], "group_by": ["p.category"], "aggregate": [{"metric": "sum", "field": "qty"}]}}` | フィールド 'c.city' はスキーマに存在しません（結合先の条件を `filter` に書いた） | ○ |
| kimi-k2.6 | SQL | E09 | 部分集合外（※） | `SELECT c.name FROM customers c JOIN orders o ON o.customer = c._id WHERE o.status = 'キャンセル' GROUP BY c._id, c.name` | '_id' ではグループ化できません | ○ |

### 例なし

| モデル | 面 | 問 | 分類 | 出力 | 理由 | 書き直し |
|---|---|---|---|---|---|---|
| mimo-v2.6-flash | SQL | C08 | 意味的に不正 | `SELECT SUM(amount) FROM orders WHERE ordered_on BETWEEN '2026-01-01' AND '2026-01-31' AND status <> 'キャンセル'` | 問いに無い条件（キャンセル除外）を足した | — |
| mimo-v2.6-flash | IR | E02 | 部分集合外 | `{"table": "orders", "query": {"filter": {"cmp": {"field": "amount", "op": "gte", "value": 80000}}, "select": ["order_no", "customer.name"], "joins": [{"alias": "c", "field": "customer"}]}}` | 別名 'customer' は結合にありません（別名ではなくリンク列名で参照した） | ○ |
| minimax-m3.1-flash | IR | C10 | 構文エラー | `{"table":"orders","query":{"select":["status"],"group_by":["status"],"aggregate":[{"metric":"sum","field":"amount"}],"having":{"cmp":{"metric":{"metric":"sum","field":"amount"},"op":"gt","value":3000000}}}` | JSON の括弧の閉じ忘れ | ○ |
| minimax-m3.1-flash | IR | E05 | 構文エラー | `{"table":"orders","query":{"filter":{"cmp":{"field":"order_no","op":"eq","value":"O-0010"}},"joins":[{"alias":"l","field":"lines","filter":{}},{"alias":"p","field":"product","from":"l","filter":{}}],"select":["p.name","l.qty"]}}` | 空の条件 `{}` は IR の形に合わない | × |
| minimax-m3.1-flash | IR | E09 | 部分集合外 | `{"table":"customers","query":{"filter":{"link":{"field":"orders","filter":{"cmp":{"field":"status","op":"eq","value":"キャンセル"}}}},"select":["name"],"group_by":["name"]}}` | group_by には aggregate の指定が必要です | ○ |
| glm-5.1 | IR | B01 | 構文エラー | `{"table": "customers", "query": {"filter": {"all": [… {"any": [ … ]}]}], "select": ["name"]}}` | JSON の括弧の対応の誤り（`]` が 1 つ多い） | × |
| kimi-k2.6 | IR | E10 | 列・テーブル名誤り | `{"table": "order_lines", "query": {"joins": [{"alias": "p", "field": "product"}, {"alias": "o", "field": "order_ref"}, {"alias": "c", "from": "o", "field": "customer"}], "filter": {"cmp": {"field": "c.city", "op": "eq", "value": "大阪"}}, …}}` | フィールド 'c.city' はスキーマに存在しません | ○ |

## 読み方と限界

- **ばらつき**: `temperature: 0` でも同じモデル・同じ条件の再実行で 1〜2 問の出入りがあった
  （評価コードの調整前に試走した例なしの条件では、glm-5.3-flash の SQL が 48/50 だった回がある）。
  モデルあたり 50 問なので、1 問 = 2 ポイントの粒度で、±2〜4 ポイントの差は誤差の範囲と読むこと。
- **問題の難しさ**: 50 問は業務的に代表的な問いとして作ったが、単純フィルタ・複合条件・集約・ソートの
  4 カテゴリは IR・SQL とも全モデルが 9〜10 問正解した。差が出るのはリンク経由（結合）だけで、
  IR の結合の書き方（結合先の条件は `joins[].filter`・射影は `別名.列`・多段は `from`）に起因する誤りが目立った。
- **基準値に有利な判定**: 余分な列を許す・`query` で包まない出力も受ける、はどちらも IR の基準値を
  押し上げる方向に効く。SQL の改善幅は控えめに出ている。
- 生の結果（問いごとの出力・判定・エラー・書き直しの出力）は `SHIKI_SQL_EVAL_OUT` に JSON で出る。
  ここには失敗の全件だけを転記した（正解は定義上、正解の SQL の結果と一致したもの）。
