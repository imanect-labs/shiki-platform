# イベント/キュー基盤の再評価 — Apache Iggy 導入検討（ADR）

- **ステータス**: **Apache Iggy は見送り（条件付きで再評価）** — 2026-08-05 に human 合意。
  併せて「将来ブローカを入れる場合の第一候補は Iggy ではなく **NATS JetStream**」も合意（§4・§6）。
  「Postgres で無理やり」の課題感は実在し、うち 1 件は実バグだったため別 issue で対処する
  （§2.1 → **F1 = #413 実装済み**・§5）。
- **日付**: 2026-08-05
- **関連**: design.md §4.3（ジョブキュー/outbox の採用根拠）・requirements.md NFR-2/4/9/10・
  roadmap/phase-10.md Task P10-A0・docs/workflow/engine.md §5.5
- **対象コード**: `crates/jobq`・`crates/storage/src/event/`・`crates/storage/src/outbox_gc.rs`・`crates/rag/src/pipeline/`・
  `crates/api/src/workflow_runtime/`・`crates/api/src/miniapp_triggers.rs`・
  `crates/app-gateway/src/routes/events.rs`・`crates/chat/src/store/stream.rs`
- **正本との関係**: 設計を変えるのは design.md。本書は比較検討と実測の記録として残す。

> **要旨**: 現状の課題の大半は「**ログが欲しい**」のではなく「**GC が本番未配線のまま
> anti-join で全表走査している**」「**消費経路が 2 系統に分裂している**」「**全経路がポーリング**」という
> 実装課題である。Iggy は追記ログとしては優れているが、(a) 現状の痛点の主要因を解かない、
> (b) クラスタリング未実装（単一ノード）で NFR-9 の可用性/整合スナップショット要件と衝突、
> (c) キュー意味論（visibility timeout / DLQ / 遅延再配信）を持たないため `jobq` は残る、
> (d) テナント単位消去（NFR-10）が追記ログでは構造的に困難、という 4 点で見送りが妥当。

---

## 1. 現状：Postgres が担っているイベント/キュー機構

「1 つの機構」ではなく、**5 系統が併存**している。ここが「無理やり感」の出発点。

| # | 機構 | テーブル | 消費方式 | 起床 | 用途 | 所在 |
|---|------|---------|---------|------|------|------|
| 1 | outbox（破壊的 ack） | `storage_event_outbox.processed_at` | `FOR UPDATE SKIP LOCKED` ＋ `processed_at` 更新 | 500ms poll | RAG 増分索引 relay | `rag/src/pipeline/relay.rs` |
| 2 | outbox（配送台帳） | `storage_event_outbox` ＋ `outbox_delivery` | `NOT EXISTS(delivery for me)` anti-join ＋ SKIP LOCKED | 5s poll | workflow イベントトリガ／miniapp B2 関数 | `storage/src/event/delivery.rs::claim_undelivered` |
| 3 | ジョブキュー | `job_queue` / `job_queue_dead` | `visible_at` ＋ vt ＋ attempts ＋ DLQ | 300〜500ms poll | RAG ingest・chat run | `crates/jobq` |
| 4 | ライブテール（SSE） | `storage_event_outbox`（読み取り専用） | `id > cursor` ＋ `payload ? 'event_type'` | **接続ごと 1s poll** | ミニアプリ `events.subscribe` | `app-gateway/src/routes/events.rs` |
| 5 | run イベント | `generation_event` ＋ Redis pub/sub | seq replay（DB＝正・pubsub＝起床） | Redis 起床＋700ms 保険 poll | chat SSE ストリーム | `chat/src/store/stream.rs` |

加えて workflow-engine は run/step の claim・lease・fencing・`effect_journal`・`run_checkpoint` を
別系統として持つ（Durable Execution なので、ここは意図どおり Postgres が正しい）。

**設計意図としては筋が通っている**（design.md §4.3）:
outbox＝ドメイン書込と同一 txn の耐久ログ兼 fan-out 点 ／ `job_queue`＝per-consumer 配送機構。
拡張依存ゼロ（pgmq 不採用）で持込 Postgres・マネージド PG・エアギャップ全対応。
問題は意図ではなく、**#2 の実装コストと、#1〜#5 が別物として増殖したこと**。

---

## 2. 課題の棚卸し（実測付き）

### 2.1 【最重要・実バグ】outbox が GC されず、`claim_undelivered` が全表走査で線形劣化

**`gc_delivered()` は実装・テスト済みだが、本番コードから一度も呼ばれていない。**

```
$ rg gc_delivered --type rust
crates/storage/src/event.rs:254:pub async fn gc_delivered(      # 定義
crates/storage/tests/outbox_fanout_it.rs:213,259               # テストのみ
```

（上記は F1 修正前の状態。F1 で `event.rs` は `event/{mod,delivery,gc}.rs` に分割し、
バックグラウンド実行は `outbox_gc.rs` に置いた。）

呼び出し元は結合テストだけで、`crates/api` の wiring にも定期ジョブにも存在しない
（`register_consumer` は `workflow_runtime/mod.rs:253` と `miniapp_triggers.rs:38` から呼ばれているのに、
対になる GC が配線漏れしている）。**つまり `storage_event_outbox` は永久に増え続ける。**

これが `claim_undelivered` の構造と噛み合って最悪の形になる:

```sql
WHERE NOT EXISTS (SELECT 1 FROM outbox_delivery d
                  WHERE d.consumer = $1 AND d.event_id = o.id)
ORDER BY o.id LIMIT $2
```

定常状態（コンシューマが追いついている状態）では未配送行は**テール＝最大 id 側にだけ**存在する。
一方このクエリは `ORDER BY o.id` の昇順なので、**配送済みの全行を先に走査してから**テールに到達する。
GC が効いていないので走査長 = 累積イベント総数。皮肉なことに、**コンシューマが健全であるほど
無駄走査が長くなる**（遅れているときだけ手前で当たる）。

#### 実測（PostgreSQL 16.13・既定 `shared_buffers`＝128MB・本番と同一のクエリ形状／スキーマ）

| outbox 行数 | `claim_undelivered` 実行時間 | バッファ |
|---:|---:|---|
| 10,000 | **3.2 ms** | shared hit=266 |
| 100,000 | **34.2 ms** | shared hit=2,497 |
| 500,000 | **141.4 ms** | shared hit=11,543 read=954 |
| 1,000,000 | **524.2 ms** | shared 24,861 blk ＋ **temp 34,478 blk**（ハッシュ/ソートが temp に溢れる） |

行数に対してほぼ線形、100 万行で 1 回の claim に **0.5 秒**。再現手順は §7 附録。

#### 影響（性能だけの話で済まない）

- 台帳コンシューマは **`workflow` と `miniapp-functions` の 2 つ**、いずれも **5 秒 tick**
  （`default_tick_secs() = 5`）。100 万行時点で 5 秒ごとに ~230MB の I/O を空振りで焼く。
- さらに悪いのは **workflow のリーダー tick が直列**であること。`relay_events()` は
  `wake_due_timers` / `expire_due_waits` / `drain_cancel_requested` / `promote_queued_runs` /
  `expire_run_timeouts` と**同一ループ内で順に**呼ばれる（`workflow_runtime/mod.rs:258-295`）。
  outbox 走査が伸びると **workflow のタイマー起床・wait タイムアウト回収・ユーザーキャンセルの
  反映が丸ごと遅延する**。単なる遅さではなく、engine.md が約束する起床遅延の上限が崩れる。
- `register_consumer` の fast-forward（初回のみバックログを配送済みに刻む）が、
  GC されない表に対して**全行 INSERT** になる。100 万行なら 100 万行の台帳挿入が起動時に走る。

#### 改善余地の実測（同じ 100 万行・比較用に `SELECT o.id` へ揃えた同一形状で A/B）

| 案 | 実行時間 | 備考 |
|---|---:|---|
| 現状 | 205 ms | 基準 |
| 走査窓を遅延ウォーターマークで限定（`id > max(delivered)-10000`） | **54 ms** | 3.8x。安全余裕付きなので「未コミット飛び越し」を起こさない |
| GC 済み（未配送 5,000 行のみ残る状態を模擬） | **8 ms** | **26x**。効くのは圧倒的にこちら |

→ **効くのは GC の配線**。走査窓の限定は補助。Iggy でもなんでもなく、**呼ばれていない関数を呼ぶこと**が最大の改善。

### 2.2 消費経路が 2 系統に分裂している（`processed_at` と `outbox_delivery`）

P10-A0（phase-10.md Task P10-A0）で台帳方式を追加したが、**既存 RAG relay は `processed_at` 経路のまま温存**した
（「挙動・テスト不変」を優先した判断・当時は妥当）。結果:

- 同じ outbox 行に対し**破壊的 ack と非破壊的台帳の 2 種類の消費**が同居する。
- `gc_delivered` は「`processed_at IS NOT NULL` **かつ** 全台帳コンシューマ配送済み」の AND 条件になり、
  **2 つの独立した進行度が揃うまで消せない**（片方が遅れると全体が溜まる）。
- 新コンシューマの追加ごとに `register_consumer` の fast-forward 手当てが必要
  （＝過去イベントで一斉発火する既定の危険を、都度の作法で避けている）。
- コンシューマ集合が**呼び出し側 wiring の引数**（`gc_delivered(&[&str])`）として渡されていた。
  これは単なる二重管理ではなく**危険**である（#413 の実装中に判明）:
  台帳コンシューマは**独立したフィーチャフラグの背後**で起動する（`workflow` は
  `workflow.enabled`・`wiring.rs:383` ／ `miniapp-functions` は `gateway.enabled`・
  `wiring_gateway.rs:301`）。「このプロセスが spawn した集合」を GC に渡すと、
  `workflow.enabled=false` のレプリカが `["miniapp-functions"]` だけを見て
  **workflow が未配送のイベントを削除する＝イベント喪失**になる。
  永続化された登録台帳（`outbox_consumer`）だけが「この配備で配送を待つべきコンシューマ」の正本。
  → F1 でこれを正した（`gc_delivered_registered`）。

これは**ログ＋per-consumer offset なら本質的に不要**になる部分で、Iggy の主張が最も刺さる箇所ではある（§3.2）。

### 2.3 SSE ライブテールが「接続数 × 1 QPS」で outbox を叩く

`app-gateway` の `events.subscribe` は **SSE 接続ごとに独立して 1 秒ポーリング**する
（`POLL_INTERVAL = 1s` / `peek_app_events_after`）。同時接続 1,000 で 1,000 QPS。
`(tenant_id, id)` の複合索引も `payload ? 'event_type'` の部分索引もないため、
主キー範囲走査＋行フィルタで済むうちは軽いが、**接続数に線形**という形が悪い。

`crates/chat` は既に **「DB＝真実のソース／Redis pub/sub＝best-effort 起床」** の正しい形を採っている
（`chat/src/store/stream.rs`）。`app-gateway` はその横展開がされていないだけ。

### 2.4 全経路がポーリング＝レイテンシ床とアイドル空回り

RAG は relay 500ms ＋ consumer 500ms なので、書込から索引反映までの**最短が約 1 秒**（実処理前）。
workflow のイベントトリガは 5 秒 tick。アイドル時も全レプリカ分のループが空クエリを回す。

### 2.5 hot table の MVCC 膨張

1 イベントあたり `INSERT` → `UPDATE (processed_at)` → `DELETE (GC)`＝**3 回書き込み**＋台帳の
`INSERT`／CASCADE `DELETE`。`storage_event_outbox` は書込チョークポイントの真下にある最ホットな表で、
autovacuum 依存が強い。パーティション化されていないため GC も行単位 `DELETE`（＝さらに膨張）。

### 2.6 replay ができない

GC 後のイベントは消える。だから新コンシューマは**バックフィルできず** `register_consumer` で
fast-forward するしかない。「イベントを再処理して索引を作り直す」「新機能を過去イベントから立ち上げる」が
構造的に不可能。ここは追記ログの土俵。

---

## 3. Apache Iggy の評価

### 3.1 事実確認（2026-08 時点）

| 項目 | 状況 |
|---|---|
| ASF ステータス | **Incubating**（2025-02-04 開始）。TLP 卒業は提案時点で「1〜2 年」見込み＝2026 末〜2027 前半 |
| クラスタリング | **未実装。現状は単一ノード**。Viewstamped Replication (VSR) を `server-ng` で実装中（DST 検証中）だが **production-ready ではない** |
| 実装/性能 | Rust・io_uring。ベンチを first-class に据え「毎秒数百万メッセージ・マイクロ秒レンジ」を主張 |
| トランスポート | QUIC / TCP（独自バイナリ） / WebSocket / HTTP REST |
| 機能 | consumer group・サーバ側メッセージ重複排除・retention/expiry・TLS・AES-256-GCM 暗号化 |
| **持たない機能** | **トランザクション**・**遅延/スケジュール配信** |
| SDK | Rust は成熟（crates.io・低/高レベル API）。C#/Java/Python/Node/Go あり、C++ 進行中 |

### 3.2 §2 の課題に効くか

| 課題 | Iggy で解決するか | 補足 |
|---|---|---|
| 2.1 GC 未配線 × 全表走査 | **✗ 解決しない** | 原因は「呼ばれていない GC」。Iggy を挟んでも `outbox` 表は**原子性のために残る**（後述）ので同じ問題が残存する |
| 2.2 消費経路の二重化 | **◎ 効く** | ログ＋per-consumer offset が正解の形。ただし Postgres 内でも台帳一本化で同等に解ける |
| 2.3 SSE の接続数比例 poll | △ 部分的 | ブローカ購読でも SSE 接続への fan-out は結局 in-process broadcast が必要。単一 tailer＋broadcast で新規依存なしに解ける |
| 2.4 ポーリングのレイテンシ床 | ○ 効く | ただし `LISTEN/NOTIFY` や既存 Redis pub/sub でも解ける |
| 2.5 MVCC 膨張 | ○ 効く | ただし outbox は残るため半分だけ。パーティション化＋`DROP PARTITION` でも解ける |
| 2.6 replay 不能 | **◎ 効く（Iggy 固有の価値）** | retention 内の offset 巻き戻し。Postgres で同等をやるならパーティション保持で近似 |

**6 件中、Iggy でしか解けないのは 2.6 の replay のみ。最重要の 2.1 には無効。**

### 3.3 導入コストとリスク

1. **outbox は消えない**。Postgres への**ドメイン書込と同一 txn**でイベントを確定させるのが
   `emit_on()` の存在理由（不変条件：単一チョークポイントで整合を担保）。Iggy と Postgres の間に
   2PC は存在しないので、構成は必ず
   `domain write + outbox INSERT（同一 txn）` → `relay` → `Iggy publish` になる。
   **outbox 表・relay ループ・GC の全てが残ったまま、ホップが 1 つ増える。**
2. **exactly-once relay が失われる**。現状の `outbox → job_queue` は**同一 Postgres 内・単一 txn のコピー**
   なので relay 段は exactly-once。Iggy 相手では at-least-once ＋ 冪等消費が必須になる
   （消費側は既に冪等キーを持つので致命ではないが、保証が 1 段落ちる）。
3. **キューではなくログ**。visibility timeout・個別 ack・DLQ・指数バックオフ再配信・遅延配信を持たない
   （遅延/スケジュール配信は機能一覧に無い）。`jobq` の vt/attempts/DLQ と workflow のタイマーは
   **Postgres に残る**。つまり系統数は減らず、**5 系統 → 6 系統になる**。
4. **NFR-9（可用性・整合スナップショット）と衝突**。単一ノードなので cell ごとに SPOF が増える。
   さらに design §4.12 は「Postgres・blob・Qdrant・FGA の**整合スナップショット**」を要求しており、
   ここに**独立した永続ストアがもう 1 つ**入ると、PG↔Iggy 間の整合復元という新しい難問が生まれる
   （復元後に「PG にはあるが Iggy には無いイベント」が発生し得る）。
5. **NFR-10（テナント完全消去・消滅証明）と衝突**。現状は `delete from job_queue where tenant_id = $1`
   （`jobq::delete_tenant`）で消える。追記ログは**個別テナントのメッセージを狙って消せない**。
   stream/topic をテナント単位に割るのが定石だが、cell 内に多数の stream を抱える運用が増える。
   「期限付き消滅証明」を製品として謳っている以上、ここは軽視できない。
6. **NFR-4（配布容易性・部品点数最小化）に真っ向から反する**。デプロイ部品は既に
   Postgres・Qdrant・OpenFGA・Keycloak・MinIO・Redis の **6 プロセス**（＋プロセス内 Tantivy）。
   7 つ目のステートフル部品を、**cell＝顧客ごとに 1 インスタンス**
   （design §4.1.1・`deploy/` の OpenTofu で cell＝モジュールのインスタンス化）追加することになる。
   バックアップ・監視・アップグレード・障害対応が cell 数だけ増える。
7. **cell トポロジではスループット要件が立たない**。Iggy の売りは毎秒数百万メッセージだが、
   1 cell = 1 社の文書書込イベントは**毎秒数件オーダー**。§2.1 の実測が示すとおり、
   現状のボトルネックは**スループットではなく走査長**。Iggy の強みが効く軸に課題が無い。
8. **Incubating を基幹データ経路に置く説明責任**。エンタープライズ/オンプレ持込みの審査で
   「メッセージ基盤は ASF インキュベーション中・クラスタリング未実装」は通しにくい。

### 3.4 結論

**現時点では見送り。** Iggy が悪いのではなく、**この製品の現在の課題形状に合っていない**。
最重要課題（§2.1）に無効で、キュー意味論を持たないため系統数を増やし、
単一ノード制約が NFR-9/10 と正面衝突する。

---

## 4. 代替案の比較

| 案 | 部品追加 | 2.1 | 2.2 | 2.4 | 2.6 replay | キュー意味論 | HA | ライセンス/成熟 |
|---|---|---|---|---|---|---|---|---|
| **A. Postgres 据置き＋構造改善（推奨）** | なし | ◎ | ◎ | ○ | △ | ◎ 既存 | PG の HA に相乗り | ◎ |
| B. Apache Iggy | +1 | ✗ | ◎ | ◎ | ◎ | ✗ 無し | **✗ 単一ノード** | Apache 2.0／**Incubating** |
| C. NATS JetStream | +1 | ✗ | ◎ | ◎ | ◎ | ◎ ack_wait/MaxDeliver/backoff/nak-delay | ◎ Raft クラスタ | Apache 2.0・実績十分 |
| D. Redpanda / Kafka | +1 | ✗ | ◎ | ◎ | ◎ | △ 再配信は自前 | ◎ | Redpanda 中核は **BSL**（source-available・第三者への商用ストリーミング提供は不可）／Kafka は JVM 運用重量 |

補足:

- **C（NATS JetStream）は、もしブローカを入れるなら Iggy より本命**。単一バイナリ・Raft クラスタリング・
  そして**ログ意味論とキュー意味論の両方**を持つ（`ack_wait` ＝ visibility timeout 相当、
  `MaxDeliver` 到達で DLQ ストリームへ、`nak` に遅延指定＝バックオフ再配信）。
  つまり **`jobq` と outbox 配送を 1 つの部品に畳める**可能性がある — これは Iggy には無い性質。
  Rust は公式 `async-nats` が Tokio ネイティブ。自己ホスト可＝エアギャップ成立。
  ただし §3.3 の 1・4・5・6（outbox は残る／整合スナップショット／テナント消去／部品点数）は
  **C でも同じく発生する**。したがって「ブローカを入れるか」の判断は A を先にやってから。
- **D の Redpanda** は中核が BSL 1.1（4 年後に Apache 2.0 へ変換）。**自己ホストの内部利用は可**だが、
  オンプレ製品への同梱・再配布と「第三者への商用ストリーミング/キューイングサービス提供の禁止」条項が
  SaaS 事業と干渉しないかの法務確認が必須。`vendor/` の fork-policy とも別次元の検討が要る。

---

## 5. 推奨アクション（Postgres のまま構造を直す）

優先度順。**F1 は不具合修正なので Iggy の議論と独立に着手すべき。**

| ID | 内容 | 効果 | 規模 |
|---|---|---|---|
| **F1** ✅ | **実装済み（#413）**。`storage::outbox_gc::spawn_outbox_gc` を wiring から起動（60 秒周期・`SKIP LOCKED` でバッチ刻み＝全レプリカ同時実行安全・リーダー選出不要）。コンシューマ集合は `outbox_consumer` 表から読む（後述の安全性の要請）。廃止用 `unregister_consumer` と滞留観測 `outbox_backlog` を追加 | §2.1 を解消（実測 **26x**） | 小 |
| **F2** | 消費経路の一本化：RAG relay も台帳コンシューマ（`consumer='rag'`）へ寄せ、`processed_at` 経路を廃止。GC 条件を台帳 AND のみに単純化 | §2.2 解消・GC 条件が 1 軸に | 中 |
| **F3** | `claim_undelivered` に**遅延ウォーターマーク**で走査窓を限定（未コミット飛び越しを起こさない安全余裕付き。実測 3.8x）。F1 と併用 | §2.1 の二重の安全網 | 小 |
| **F4** | SSE を**単一 tailer ＋ `tokio::sync::broadcast`** に変更（接続数に依らず 1 QPS）。`(tenant_id, id)` 索引と `payload ? 'event_type'` 部分索引を追加 | §2.3 解消 | 中 |
| **F5** | 起床を `pg_notify`（または既存 Redis pub/sub）に。**DB＝正・pubsub＝best-effort 起床**という `chat` の実績パターンをそのまま横展開し、ポーリングは保険として残す | §2.4 解消（新規依存ゼロ） | 中 |
| **F6** | `storage_event_outbox` を時間パーティション化し、GC を `DROP PARTITION` へ。retention 内はイベントが残るので**限定的な replay** も得られる | §2.5 解消・§2.6 を部分的に | 中 |
| **F7** | `EventLog` トレイト境界を切る（`publish` / `subscribe(consumer, from)`）。実装は当面 Postgres 一本 | 「差し替えはトレイト裏で」原則に沿って**将来 Iggy/NATS への退路**を確保 | 小〜中 |

F1〜F6 を入れた後の姿は「1 系統の outbox（パーティション・台帳消費・notify 起床）＋ `jobq`」で、
**部品追加ゼロ・系統数 5 → 2**。§2 の 6 課題のうち 5.5 件が解消する。

---

## 6. Iggy（またはブローカ）を再評価する条件

以下のいずれかが成立したら本書を破棄して再検討する。

1. **F1〜F6 実施後もなお** Postgres のイベント経路が容量/レイテンシのボトルネックである（要測定）。
2. cell あたりのイベントレートが**継続的に数千 msg/s** を超える（現状は毎秒数件オーダー）。
3. **フルプール化**（roadmap SAAS.5）で全テナントのイベントが単一 Postgres に集中する。
4. **外部へのイベント配信が要件化**する（監査ストリーム外販・顧客システムへの CDC・
   parallel-tracks SK.7 の利用量集約イベントが「集約値の push」を超えてストリーム連携になる場合）。
5. Iggy が **TLP 卒業 かつ クラスタリング（VSR）が production-ready** になる。
   ただしその時点でも、キュー意味論を持つ **NATS JetStream の方が本製品には適合度が高い**可能性が高い（§4）。

---

## 7. 決定事項と残タスク

### 決定済み（2026-08-05）

- ✅ **Iggy は見送り**（§3.4）。再評価条件は §6。
- ✅ **将来ブローカを入れる場合の第一候補は NATS JetStream**（§4 C）。Iggy はクラスタリングが
  production-ready になった時点で再検討対象に戻すが、キュー意味論を持たない以上、
  `jobq` を畳める NATS の方が本製品への適合度が高い。
- ✅ **F1（GC 配線）は不具合として即修正**（#413・実装済み）。

### 残タスク（human 確認事項）

1. **F2（消費経路の一本化）** は RAG relay の挙動を変える（既存テストに影響）。
   P10-A0 が「挙動不変」を優先して意図的に温存した箇所なので、方針変更の合意が要る。
2. **F3〜F7 の優先順位と実施タイミング。** F4（SSE の単一 tailer 化）は接続数比例の
   ポーリングを消すので、ミニアプリの同時利用が増える前に入れたい。
3. 本書の結論を **design.md §4.3 に 1 段落追記**（「ブローカ非採用の再確認と再評価条件」）し、
   本書を背景記録として参照させたい。
4. **別件（本 ADR の範囲外・要判断）**: `migrations/` に **version 57 が重複**している
   （`0057_node_system.sql`（#396）と `0057_share_link_grant_revoke.sql`（#393）が別 PR から
   同番で入った）。**新規 DB は `sqlx::migrate!` が `VersionMismatch(57)` で失敗し、マイグレート
   できない**（既存 DB は増分適用済みなので露見しない。#413 のテストを回す際に踏んだ）。
   新規 cell プロビジョニング（NFR-11）と新規開発環境に効く。既存 DB の適用履歴を壊さない
   リナンバー手順が必要なので、別 issue で方針を決めたい。

---

## 附録：§2.1 実測の再現手順

PostgreSQL 16.13・既定設定（`shared_buffers=128MB`）・ローカルディスク。
**絶対値ではなく行数に対する増加傾向**を見るための測定。

```sql
-- 本番と同一のスキーマ・索引（migrations 0002 / 0021 相当。node は LEFT JOIN 相手として最小構成）
create table node (id uuid primary key, system boolean not null default false);
create table storage_event_outbox (
    id bigserial primary key, org text not null, tenant_id text not null,
    node_id uuid not null, version bigint not null, op text not null, actor text not null,
    trace_id text, payload jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now(), processed_at timestamptz);
create index storage_event_outbox_unprocessed_idx
    on storage_event_outbox (id) where processed_at is null;
create table outbox_delivery (
    consumer text not null,
    event_id bigint not null references storage_event_outbox (id) on delete cascade,
    tenant_id text not null, delivered_at timestamptz not null default now(),
    primary key (consumer, event_id));
create index outbox_delivery_event_idx on outbox_delivery (event_id);
```

イベントを積みつつ「**テール 5 件を除く全行を配送済み**」にして（＝GC されず溜まった実運用状態）、
`storage/src/event.rs::claim_undelivered` と同一形状のクエリを `EXPLAIN (ANALYZE, BUFFERS)` する。
行数を 1 万 → 10 万 → 50 万 → 100 万と伸ばし、各段で `ANALYZE` 後に測定。
