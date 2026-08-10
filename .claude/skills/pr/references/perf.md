# パフォーマンス評価

CLAUDE.md のコーディング規約（「パフォーマンスを追求する」「全件取得→フィルタではなく、最初から必要なデータ・フィールドのみ取得する」）を PR ゲートとして実際に効かせる。

このプロダクトにはまだ数値 SLO（p95 レイテンシ等）が無い（`docs/requirements.md` §4 の NFR は隔離・可搬性・監査性が中心）。SLO の策定は負荷条件の定義とベースライン測定を伴う別タスク。**それが決まるまでの間も「元から遅い箇所」を見つけられるよう**、評価は 3 層で行う:

| 層 | 何を見るか | 出力 |
| --- | --- | --- |
| **A. 静的観点** | diff に劣化パターンが無いか | この PR で直す |
| **B. 相対回帰** | base と比べて悪化していないか | 悪化なら直す |
| **C. 絶対観測** | 変更領域の実測値が単体で見て遅くないか | この PR の範囲なら直す。**範囲外の既存の遅さなら issue に切る** |

C を入れる理由は、A と B だけだと**元から遅いものが永久に温存される**ため（差分が無いので相対評価に映らず、静的パターンにも当たらない）。

C の閾値は**合否ゲートではなく調査トリガ**。超えたら落とすのではなく「なぜそうなっているか調べる」。数値は暫定で、SLO 策定時に実測ベースへ置き換える。

## A. 静的観点（全 PR で見る）

diff を上から読み、次のパターンに当たったら指摘し、この PR で直す。直さない判断をしたなら理由を PR 本文に書く。

### データ取得

- **全件取得 → メモリでフィルタ。** `SELECT` の後に `.iter().filter()` / `.retain()` で絞っているものは、`WHERE` へ落とす。ページングも DB 側（`LIMIT`/`OFFSET` かキーセット）で行う。
- **`SELECT *` 相当。** 使わない列（特に本文・BLOB・JSONB の大きい列）を引いていないか。必要なフィールドだけを列挙する。
- **N+1 クエリ。** ループ内で `await` するクエリは、`WHERE id = ANY($1)` の一括取得＋メモリ結合に畳む。
- **N+1 の認可チェック。** ループ内で `AuthzClient::check` を呼んでいないか。
  - ⚠️ **RAG の post-filter は例外。除去しない。** design.md §4.3 は「post-filter は reranker の前・**file 粒度**の OpenFGA check（HigherConsistency・剥奪の即時反映＝PIT-11）」を**必須**と定めており、`crates/rag/src/authz_filter.rs` の `post_filter_by_file` が distinct file ごとに check を並列で回すのは**仕様どおり**。これを「N+1 だから」と pre-filter に置き換えると、二段 authz が一段に退化する（= セキュリティ後退）。二段 authz は「片方が壊れても権限を守る」ためにあり、性能を理由に片方を落とさない。
  - 最適化するなら、段を減らすのではなく **1 段の中で**まとめる（distinct 化・並列化・粒度の見直し）。
  - `list_objects` による pre-filter を新規に書く場合は、**カーディナリティ上限と縮退**をセットで実装する。design.md §4.3 は「可読集合が上限 500（ListObjects の応答上限 1000 未満）を超えたら pre-filter を放棄して **tenant-only へ縮退**し post-filter 全依存＋over-fetch 引き上げ」と定める（`rag/src/config.rs` の `readable_tags_max`）。上限を設けないと、切り詰められた不完全集合を正として使い、**読めるはずのオブジェクトが無言で消える**（エラーにならないので検知もされない）。
- **インデックスの有無。** 新しく `WHERE` / `ORDER BY` / `JOIN ON` に使う列の組み合わせに、対応するインデックスが `migrations/` にあるか。無ければ同じ PR で migration を足す（番号の並行 PR 衝突に注意・`gates.md` 参照）。
- **カウントのための全件取得。** 件数だけ要るなら `COUNT(*)`。存在確認だけなら `EXISTS` / `LIMIT 1`。

### 並行性

- **直列 `await` の並列化余地。** 互いに依存しない複数の I/O が順に `await` されていたら `tokio::try_join!` / `futures::future::try_join_all` にする。特に「DB 取得 → FGA チェック → オブジェクトストア」の組み合わせ。
- **並列度の無制限化。** `try_join_all` に可変長のコレクションを渡すと、要素数ぶん同時に走って DB プールや外部 API を飽和させる。`buffer_unordered(N)` で上限を付ける。
- **ロック保持中の `await`。** `Mutex`/`RwLock` のガードを持ったまま I/O を待っていないか。

### コピー

- **不要な `clone()` / `to_vec()` / `to_string()`。** 特にループ内と、大きな `String`（ドキュメント本文・生成テキスト）・`Vec<f32>`（埋め込みベクトル）・チャンク集合に対するもの。参照・`Cow`・`Arc` で足りないか。
- **境界をまたぐ再シリアライズ。** 同じ JSON を何度も `to_string` / `from_str` していないか。

### ストリーミング / SSE

- **SSE のバッファリング。** ストリーム経路を触ったら、レスポンスヘッダの `Cache-Control: no-transform` が残っているか確認する（**これが無いと Next の BFF がバッファして、ストリームが最後まで届かない**）。中間のプロキシ・変換で溜め込んでいないか。
- **チャンク単位。** トークン 1 個ごとに DB 書き込み・イベント発行していないか（バッチ/デバウンス）。

### LLM 呼び出し

- **直列化された LLM 呼び出し。** 独立した生成・要約・埋め込みは並列化する（レイテンシが支配的なため効果が大きい）。
- **プロンプトへの丸ごと投入。** コンテキストに全チャンク・全履歴を入れていないか（トークンコストとレイテンシに直結）。

### web

- **不要な `"use client"`。** サーバコンポーネントで済むものをクライアントに落としていないか。
- **重いライブラリの静的 import。** エディタ（TipTap / GrapesJS / glide-data-grid）・チャート（recharts）・地図（MapLibre）は `next/dynamic` で遅延させる。
- **再レンダリングの誘発。** レンダーごとに新しいオブジェクト/配列/関数を props に渡していないか。リストの `key` にインデックスを使っていないか。
- **クライアント側の全件フェッチ → 絞り込み。** サーバ側のクエリパラメータで絞る。

## B. 相対回帰（base との比較）

変更領域が該当する時だけ回す。数値は PR 本文の「## パフォーマンス」に書く。

### SQL を追加・変更した

`gates.md` のテスト用 DB（pg-test `:55432`）に対して実行計画を見る:

```bash
docker exec -i pg-test psql -U postgres -d shiki -c \
  "EXPLAIN (ANALYZE, BUFFERS) <対象クエリ>;"
```

見るポイント: `Seq Scan` が想定外のテーブルに出ていないか / `rows` の見積と実測が桁で乖離していないか / `Nested Loop` の内側が毎回スキャンになっていないか。データ量が少ないローカルでは Seq Scan が選ばれるのが正常なので、**プランナが使えるインデックスが存在するか**を主眼にする（`\d <table>` で確認）。

### `web/` を変更した

`next build` の Route ごとの First Load JS を base と比較する。

2 つの前提に注意する:
- **稼働中の dev サーバと同じ worktree でビルドしない**（`.next` を壊す・`verify.md` の罠）。
- **`git stash` で base に戻さない。** 変更が無い時に `stash` は何も積まず、`stash pop` が
  **無関係な古い stash を取り出す**。base 側は別 worktree でビルドする。

**HEAD 側も別 worktree でビルドする。** カレントで `pnpm build` すると、`dev-up.sh` で起動した
`next dev`（:3000）の `.next` を上書きして壊す（上の 1 つ目の注意はここにも掛かる）。

```bash
BASE=$(git symbolic-ref --short refs/remotes/origin/HEAD 2>/dev/null); BASE=${BASE:-origin/main}
TMP=$(mktemp -d)
git worktree add "$TMP/base" "$BASE"
git worktree add "$TMP/head" HEAD
for side in base head; do
  (cd "$TMP/$side/web" && pnpm install --frozen-lockfile && pnpm build) > "/tmp/$side-build.txt" 2>&1
done
diff <(grep -E '[○ƒλ●]' /tmp/base-build.txt) <(grep -E '[○ƒλ●]' /tmp/head-build.txt)
git worktree remove "$TMP/base" --force; git worktree remove "$TMP/head" --force
```

未コミットの変更も測りたいなら、先にコミットするか `git stash` ではなく
`git worktree add` 先へパッチを当てる（`git stash` は変更が無い時に無関係な stash を pop する）。

数十 KB 単位で増えていたら、遅延 import の漏れか、意図しない依存の巻き込みを疑う。

### `crates/sandbox-*` を変更した

3 ティア横断のライフサイクル実測（gated バイナリ）:

```bash
SANDBOX_BENCH=1 cargo run --release -p shiki-sandbox-bench --bin sandbox-bench
```

### エージェント・ワークフローの実行系を変更した

`Spent`（トークン消費）は **per-step 累積のため二次で増える**。ステップ数の増加は見た目以上にコストへ効く。ステップ数・並列度を変えたら、実行 1 本ぶんの合計トークンを before/after で比較する。

## C. 絶対観測（単体で見て遅くないか）

**base と同じでも遅いものは遅い。** A・B は差分しか見ないので、既存の遅さを構造的に見逃す。触った領域については、実測値を単体で評価する。

以下は**調査トリガの目安**であって合否基準ではない。ローカル（小データ・stub LLM）での値なので、超えたら「なぜか」を調べる。原因が分かってこの PR の範囲なら直し、**範囲外なら issue に切って PR 本文に番号を書く**（黙って見なかったことにしない）。

| 観測対象 | 調査トリガの目安 | 測り方 |
| --- | --- | --- |
| 同一クエリの反復回数 | 表示件数と**同じ回数**繰り返されていたら N+1 | 下記のクエリログ |
| 単一クエリの実行時間 | ローカルの小データで **50ms 超** | クエリログの `elapsed_secs` / `EXPLAIN (ANALYZE)` |
| API 応答（LLM を挟まない経路） | **300ms 超** | リクエストログの `latency_ms` |
| Route の First Load JS | **300KB 超** | `next build` の出力 |
| 画面遷移・操作の体感 | **1 秒以上待たされる** | Phase 2 のスクショ/動画を撮る過程で気づく |

### SQL を実測する（N+1 の検出・遅いクエリの特定）

sqlx はデフォルト設定なので、`RUST_LOG` にレベルを足すだけでクエリログが出る。N+1 はループが関数をまたぐと静的に見つけられないので、**疑わしい画面は実際に測る**のが確実。

```bash
LOG=${TMPDIR:-/tmp}/shiki-dev-up-$(id -u)/shiki-server.log
# dev-up.sh が生成する run-server.sh の RUST_LOG を書き換えて起動し直す
sed -i 's|^export RUST_LOG=.*|export RUST_LOG="info,sqlx::query=debug"|' \
  ${TMPDIR:-/tmp}/shiki-dev-up-$(id -u)/run-server.sh
```

出力は 1 クエリ 1 行の JSON で、`fields.summary`（クエリ）・`fields.elapsed_secs`・`fields.rows_returned` を持つ。

> **⚠️ 単純なカウントは使えない。** バックグラウンドのワーカー（jobq・ワークフロー実行・スケジューラ）が
> **ユーザー操作ゼロでも常時ポーリングしている**（実測: アイドル時 約 35 クエリ/秒。内訳は
> `UPDATE step_execution` ≒20/s、`update job_queue set visible_at` ≒13/s）。
> さらに **sqlx のログ行は HTTP リクエストの span を持たない**ため、リクエスト単位の帰属もできない。
> したがって「1 リクエストで N 本」を直接数えることはできない。

**N+1 の検出**は、総数ではなく「**同一クエリの反復**」を見る。これはバックグラウンドノイズに強い:

```bash
B=$(wc -l < "$LOG")
#   ← ここで対象の画面を 1 回だけ操作する
sed -n "$((B+1)),\$p" "$LOG" | grep '"target":"sqlx::query"' \
  | jq -r '.fields.summary' \
  | grep -vE 'step_execution|job_queue|concurrency_counter|workflow_run' \
  | sort | uniq -c | sort -rn | head
```

同じ `summary` が**表示件数と同じ回数**並んでいたら N+1。`WHERE id = ANY($1)` の一括取得に畳む。
`grep -vE` の除外リストはポーラーを落とすためのもので、これも実態に合わせて更新する。

**遅いクエリの特定**は `elapsed_secs` で引く（バックグラウンド分も含めて全体を見られる）:

```bash
grep '"target":"sqlx::query"' "$LOG" \
  | jq -r 'select(.fields.elapsed_secs > 0.05) | "\(.fields.elapsed)\t\(.fields.summary)"' \
  | sort -rn | head -20
```

### API 応答時間を見る

サーバは全リクエストの `latency_ms` を構造化ログに出している:

```bash
grep '"path":"/<対象パス>"' /tmp/shiki-dev-up-$(id -u)/shiki-server.log \
  | grep -oE '"latency_ms":[0-9.]+' | sort -t: -k2 -n | tail -5
```

LLM・サンドボックス・外部 API を挟む経路は当然長くなるので対象外。**DB とストレージだけで完結する経路**を見る。

### 既存の遅さを見つけた時

- この PR の変更範囲内 → 直す。
- 範囲外 → **issue に切る**（`area:*` ラベル・再現手順・実測値・`path:line` を書く）。PR 本文の「## パフォーマンス」に issue 番号を残す。
- 「気づいたが放置」はしない。スコープを広げないことと、見なかったことにすることは違う。

## D. 出力

- 静的観点（A）で見つけたものは、**この PR で直す**のが既定。
- 相対回帰（B）で悪化していたら直す。実測した数値を PR 本文に書く。
- 絶対観測（C）で調査トリガを超えたものは、範囲内なら直し、範囲外なら issue 化して番号を書く。
- 直さない場合は PR 本文に理由を書く。
- 影響が無いと判断したなら「影響なし」と明記する（**空欄にしない** — 見ていないのか、見て問題無かったのかが区別できなくなる）。
