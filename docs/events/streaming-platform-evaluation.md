# イベント/キュー基盤の再評価 — Apache Iggy 導入検討（ADR）

- **ステータス**: **Apache Iggy は見送り（条件付きで再評価）** — 2026-08-05 に human 合意。
  併せて「将来ブローカを入れる場合の第一候補は Iggy ではなく **NATS JetStream**」も合意（§4・§6）。
  「Postgres で無理やり」の課題感は実在し、うち 1 件は実バグだったため別 issue で対処する
  （§2.1 → **F1 = #413 実装済み**・§5）。
- **2026-10 再評価（#818・§8）**: 旧版の §3.3 の見送り理由 4〜7 と §6 の再評価条件 2・3 は
  「cell＝顧客ごとに 1 インスタンス」を前提にしていた。SaaS の既定はフルプール（design §4.1・SAAS.5）なので、
  その前提で測り直して書き直した。**結論（ブローカ見送り・Postgres 継続）は変わらない。**
  ただし、プールで先に詰まるのはブローカの有無ではなく、次の 2 点だと分かった。
  - **コンシューマの固定周期が決めるドレイン上限**: workflow が 12.8 ev/s、RAG relay が ~128 ev/s。これを超えるとイベントトリガや索引反映が際限なく遅れ、GC が止まる。
  - **1 社の一括取込が他キュー・他社を巻き込む干渉**
  - → どちらも Postgres のまま直す（**F8＋F3 → F9①** を優先・§5）。優先順位と §6 の書き換えは human 確認待ち（§7 残タスク 5）。
- **日付**: 2026-08-05（フルプール再評価 2026-10-22）
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
> **フルプール前提でも同じ結論になる**（§8）。プールでは (b)(d) が強まる（SPOF が全テナントに及び、
> 共有ログから 1 社分を消せない）。プール全体のピーク 10²〜10³ ev/s は Postgres の経路で捌ける。
> 先に詰まるのはコンシューマの固定周期（workflow 12.8 ev/s）とテナント間の干渉で、どちらも Postgres のまま直す（F8・F9）。

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

> **4〜7 は 2026-10 にフルプール前提で書き直した（#818・実測は §8）。** 旧版は
> 「cell＝顧客ごとに 1 インスタンス」を前提にしていた。SaaS の既定は全テナント共有のフルプール
> （design §4.1・SAAS.5）なので、4・5 は**強まり**、6 は SaaS 側では**弱まり**、7 は根拠を差し替えた。

4. **NFR-9（可用性・整合スナップショット）と衝突し、プールでは影響範囲が全テナントになる**。
   Iggy は単一ノードなので、プールに 1 台置けば**全テナントのイベント経路が 1 つの SPOF にぶら下がる**
   （cell 前提では「その 1 社だけ止まる」だった）。
   さらに design §4.12 は「Postgres・blob・FGA の**整合スナップショット**」を要求しており
   （Qdrant / Tantivy は Postgres から作り直す派生索引なので対象外）、ここに
   **Postgres から作り直せない永続ストアがもう 1 つ**入ると、PG↔ブローカ間の整合復元という新しい難問が生まれる
   （復元後に「PG にはあるがブローカには無いイベント」が発生し得る。outbox は relay 後に GC されるので、
   PG 側から再送して埋めることもできない）。フルプールは既に「A 社だけ 3 日前に戻す」
   （テナント単位 PITR）を未解決課題として抱えており（design §4.12・data-platform §12.1）、
   復元対象のストアがもう 1 つ増えるとこの課題はさらに重くなる。
   NATS JetStream の Raft クラスタなら SPOF は消えるが、整合スナップショットの問題は残る。
5. **NFR-10（テナント完全消去・消滅証明）と衝突し、プールではこの衝突が強まる**（§8.5）。
   Postgres では outbox もジョブも `DELETE ... WHERE tenant_id = $1` で消せる。消去対象の表は
   `pg_catalog` から機械的に導出され（`storage::tenant_scope`・#420。権限で列が絞られる `information_schema` は意図的に使わない）、
   1 トランザクションで消える。
   cell 前提なら「その社の stream ごと捨てる」という逃げ道があったが、**プールでは使えない**。
   - **テナント単位に stream/topic を割る手は取れない**。物理オブジェクトがテナント数に比例して増えるため、
     NFR-8（「テナント数に比例して物理オブジェクトが増える設計を禁じる」）に反する。
   - そこで共有 stream に全テナントのメッセージが混ざることになる。**Iggy と Kafka には、混在した追記ログから
     1 テナント分を選んで消す直接の操作が無い**。消せる単位は segment の期限切れ、topic/stream の purge、
     offset より前の切り詰めである。近似する手段としては、テナント鍵で payload を暗号化し消去時に鍵を破棄する
     **crypto-shredding** と、キーにテナントを含めて tombstone を書き compaction で消す **Kafka の compaction＋tombstone** がある。
     ただしどちらも、消去経路（鍵管理か compaction の完了待ち）と「いつ物理的に消えたか」の証明を別に持つことになる。
     近似手段を使わずに retention を短くして期限を守る場合は、Iggy 固有の唯一の利点だった replay
     （§2.6・§3.2）と打ち消し合う。replay は長い retention を前提とするからである。
   - NATS JetStream は subject フィルタ付き purge（`events.<tenant>.>`）と、上書き消去付きのメッセージ単位削除を持つ。
     このため技術的には消せる。ただし、①`tenant_scope` の機械導出の外にある別の消去経路と、
     その CI 網羅検査を足す必要がある、②PG の purge と同じトランザクションにならないので、片方だけ消えた中間状態が生じる、
     ③ブローカのスナップショット/バックアップも消滅証明の対象に加わる、という 3 つのコストが残る。
     payload の `actor`（利用者 id）は個人データなので、「イベントだから消さなくてよい」とは言えない。
   - まとめると、5 は「ブローカでは消せない」ではなく「**消去経路・鍵・証明の対象が Postgres の外に 1 系統増える**」である。
     この論拠は近似手段がある分だけ弱い。見送りの結論そのものは 4・6・7 で支えられる。
6. **NFR-4（配布容易性・部品点数最小化）に反する。SaaS 側でのこの理由はプールで弱まった**。
   デプロイ部品は既に Postgres・Qdrant・OpenFGA・Keycloak・RustFS・Redis の **6 プロセス**（＋プロセス内 Tantivy）。
   旧版の「cell 数だけステートフル部品が増える」は、プールでは「**プールに 1 クラスタ**」になる。
   ただし負担は次の 2 つに移るだけで、消えはしない。
   - **オンプレ**（NFR-4 の主対象・compose 一発起動）。「差し替えはトレイト裏で」の原則上、
     ①オンプレにもブローカを同梱して部品を 7 つにするか、②`EventLog` トレイト（F7）の裏に
     Postgres 実装を残して二実装を保守し続けるか、のどちらかになる。②ではオンプレにブローカの利点が届かず、
     二実装の挙動差（at-least-once の境界・順序・消去）を CI で揃え続けるコストがかかる。
   - **HA 運用**: プールの SPOF を避けるには 3 ノード以上のクラスタが要る（4 を参照）。
7. **プール全体でもスループット要件は立たない。プールで先に詰まるのは、ブローカでは解けない 2 点である**（§8.3・§8.4）。
   1 社あたりの書込イベントは cell 前提の見積りどおり毎秒数件のオーダーだが、プールではその総和、
   つまり**一次目標（数十〜数百社）のピークで 10²〜10³ ev/s** を見込む（§8.2）。これは Iggy が売りにする
   毎秒数百万メッセージより 3 桁以上小さく、同じ機材で測った Postgres の経路にも収まる（§8.3）。
   実際に先に詰まるのは次の 2 点で、どちらもブローカを入れても解けない。
   - **コンシューマの固定周期が決めるドレイン上限**: workflow が 64 件/5 秒＝**12.8 ev/s**、miniapp-functions が
     50 件/5 秒＝10 ev/s/レプリカ、RAG relay が 64 件/500 ms＝~128 ev/s（§8.4）。GC は relay の ack
     （`processed_at`）と登録済み全台帳コンシューマへの配送が揃うまで行を消せないので、
     プール全体のレートがこの値を超え続けると outbox は際限なく伸び、§2.1 の劣化が再発する。
     上限はコードの `sleep` と `LIMIT` の定数から来ており、配送基盤を取り替えても変わらない。
   - **テナント間の head-of-line（HOL）**: 1 社の一括取込が FIFO で全社のイベントとジョブの前に並ぶ（§8.3）。
     ブローカに替えても、テナントごとの公平なスケジューリングは自前で作ることになる。
     テナントごとに consumer や partition を作れば、5 と同じく NFR-8 に反する。
8. **Incubating を基幹データ経路に置く説明責任**。エンタープライズ/オンプレ持込みの審査で
   「メッセージ基盤は ASF インキュベーション中・クラスタリング未実装」は通しにくい。

### 3.4 結論

**現時点では見送り。** Iggy が悪いのではなく、**この製品の現在の課題形状に合っていない**。
最重要課題（§2.1）に無効で、キュー意味論を持たないため系統数を増やし、
単一ノード制約が NFR-9/10 と正面衝突する。

**フルプール前提で再評価しても結論は変わらない（2026-10・#818・§8）。** プールでは NFR-9/10 との衝突が強まる。
SPOF が全テナントに及び、共有の追記ログからは 1 テナント分を消せないからである。
部品点数の負担（6）は SaaS 側では軽くなるが、オンプレと二実装保守の側に移るだけで消えない。
スループット（7）はプールの総和で見ても Postgres の経路に収まる。
プールで先に詰まるのはコンシューマの固定周期と HOL で、どちらもブローカでは解けない。

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
  **C でも同じく発生する**（2026-10 のプール再評価で細部を更新: 4 の SPOF は Raft クラスタで消えるが整合スナップショットは残る。
  5 は subject フィルタ付き purge で消せるが、消去経路と消去対象が 1 つずつ増える。§3.3・§8.5）。したがって「ブローカを入れるか」の判断は A を先にやってから。
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
| **F8**（2026-10 追加・#818） | **outbox コンシューマのドレイン上限を外す**: ①**RAG relay**（`processed_at` 経路）も含め、バッチが満杯なら待たずに続けて claim する（rag の ingest consumer ループが既にこの形）。②workflow の `relay_events` はリーダー tick（タイマー起床・キャンセル回収と直列・5 秒）から**独立したループ**に出す。miniapp-functions も同様にする。③outbox GC の 1 周期上限（20 バッチ/60 秒）も、満杯なら次周期を待たずに続ける。④RAG の ingest consumer（リーダー 1 本・並列 2）の並列度をプール規模に合わせて設定できるようにする。リーダー限定が順序保証のために要るかどうかを確かめたうえで、レプリカ横断の claim も検討する | §8.4 の固定上限（**RAG relay ~128 ev/s・workflow 12.8 ev/s・miniapp-functions 10 ev/s/レプリカ・GC 333 ev/s/レプリカ**）を外す。relay の ack（`processed_at`）も GC の条件なので、①を欠くと relay の上限が GC を止める。①〜③で、プールで GC が止まり §2.1 が再発する経路を断つ。④は索引反映の遅れと job_queue の滞留（→F9①の干渉）を抑える | 小〜中 |
| **F9**（2026-10 追加・#818） | **テナント間の公平性と job_queue のキュー間干渉**: ①`job_queue (queue, id)` 索引を足し、キューごとに id 順で claim できるようにする（現状は PK 走査＋`queue` フィルタなので、別キューの大量バックログを読み飛ばす）。②一括取込（オンボーディング・大量アップロード）を別キューに分けるか、claim をテナント間で公平にする（**②は #816 で実装中**: `fair_seq`・キューごとの単調仮想時刻による公平 claim） | §8.3: rag_ingest に 100 万件あると chat_generation の claim が 285 ms かかる。1 社の一括取込の後ろに他社のジョブが 99 万件待たされる | 中 |

**フルプール前提の優先順位（2026-10・#818・§8.6）**: **F8＋F3（同時）→ F9① → F4 → F2 → F5 → F6 → F7**。

- **F8 が最優先**。プール全体のレートは一次目標のピークで 10²〜10³ ev/s（§8.2）と見込むが、
  台帳コンシューマは固定周期で 12.8 ev/s、RAG relay は ~128 ev/s までしか捌けない。上回り続けると、配送が際限なく遅れ、
  GC が止まり、outbox が伸び続ける（§8.3 の `CONSUMERS=code` で再現）。
  これは性能の最適化ではなく、**プールでは機能が壊れる**問題である。
  workflow のイベントトリガが数分〜数時間遅れ、§2.1 の線形劣化も戻ってくる。
  GC の周期上限（20 バッチ/60 秒＝333 ev/s/レプリカ）も同じ形なので、F8 に含める。
  RAG relay の上限は F2（relay の台帳化）でも消えるが、F2 は合意待ちなので、F8 で relay のループ自体を直す。
  F2 が先に合意されれば、①は F2 に吸収される。
- **F3 は F8 と同時に入れる**。GC が 60 秒周期で回る限り、outbox には常に「直近 60 秒分の配送済み行」が残る。
  `claim_undelivered` はその窓を毎回頭から走査するので、1 回あたりのコストはレート R に比例し、
  呼び出し回数も R に比例する。つまり**コンシューマ 1 つあたりの DB 時間は R² で増える**
  （§8.3: R=1,000 ev/s なら 1 コンシューマで毎秒 ~0.4 秒）。F8 で上限を外すとこの二乗が表に出るので、
  走査窓を限定する F3 が要る。遅れたコンシューマが 1 つでもあると GC が全テナント分止まる、というプールの性質に対する安全網にもなる。
- **F9① は索引 1 本で済み、対話系（chat_generation）の遅延を 1 社の一括取込から切り離せる**。
  ②の公平化は設計判断を伴うので別に扱う。
- **F4** は、プールでは SSE 接続数と、イベントの少ないテナントの `latest_event_id` 全走査が効いてくる
  （§8.3: 100 万行で 247 ms）。加えて、SSE の poll は**活動中のテナントでも、自社イベントの間に挟まった他社の行を読む**
  （§8.3: T=1,000 で 2 万行・~5 ms/poll。テナント数に比例する）。**休眠テナントの SSE 接続は毎秒の poll ごとに窓全体を走査する**
  （§8.3: 100 万行で ~140 ms/poll・N に比例）。F4 はこれらを `(tenant_id, id)` 索引で 1 テナント分の範囲走査にし、
  単一 tailer 化で接続数比例の poll 自体も消す。同時に、テナント消去の outbox `DELETE` も索引で引けるようになる。
- F5・F6 の価値はプールでもほぼ変わらない。F2 は従来どおり human の合意待ち（§7 残タスク 1）。

F1〜F6 を入れた後の姿は「1 系統の outbox（パーティション・台帳消費・notify 起床）＋ `jobq`」で、
**部品追加ゼロ・系統数 5 → 2**。§2 の 6 課題のうち 5.5 件が解消する。
プールで成立させるには、これに F8（ドレイン上限の撤廃）と F9（キュー間・テナント間の干渉の除去）が要る。

---

## 6. Iggy（またはブローカ）を再評価する条件

以下のいずれかが成立したら本書を破棄して再検討する。

1. **F1〜F6・F8・F9 実施後もなお** Postgres のイベント経路が容量/レイテンシのボトルネックである（要測定）。
2. **プール全体**のイベントレートが**継続的に数千 ev/s** に達する。具体的には、F8 で全コンシューマ
   （RAG relay・workflow・miniapp-functions・GC）の固定上限を外したうえで、本番機材で測った outbox 経路の持続上限の
   **50% を、ピーク時間帯に継続して超える**こと。持続上限は、§8.3 の mixed（DB 側だけの上限）ではなく、
   **本番のコンシューマ処理を含めて**測る。workflow はイベントごとに `match_event`・`wake_event_waits` を、
   miniapp-functions は購読照会と関数起動を直列に行うので、実効上限は mixed の値より低くなる（§8.3(d) の注）。§8.2 の見積りでは一次目標（数十〜数百社）のピークは 10²〜10³ ev/s で、
   この条件には第 1 段の上端（千社規模）で初めて近づく。
   旧版はこの条件を cell あたりで書いていた（毎秒数件）。プールでは総和で判定する。
3. ~~**フルプール化**（roadmap SAAS.5）で全テナントのイベントが単一 Postgres に集中する。~~
   → **SAAS.5 で既に成立しており、#818（§8）で評価した**。結論は「ブローカ不要・Postgres のまま F8/F9 を先に入れる」。
   代わりに次を条件とする: **outbox 経路が、プール分割（data-platform §12 第 2 段）の発火指標を他の書込より先に押し上げる**。
   たとえば outbox＋台帳＋job_queue の WAL が書込ノード WAL の過半を占める場合や、イベント経路のテナント間
   p95 比が 3 倍を超える場合がこれに当たる。この場合も先に検討するのはプール分割（イベント経路もテナント単位で割れる）で、
   ブローカはその後である。
4. **外部へのイベント配信が要件化**する（監査ストリーム外販・顧客システムへの CDC・
   SaaS 課金〔SAAS.3〕の利用量連携が「集約値の push」を超えてストリーム連携になる場合。旧 parallel-tracks SK.7 の統一請求は 2026-09-29 に廃止）。
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
4. ✅ **別件（#416 で対応済み）**: `migrations/` に **version 57 が重複**していた
   （`0057_node_system.sql`（#396）と `0057_share_link_grant_revoke.sql`（#393）が別 PR から
   同番で入った）。`_sqlx_migrations` の主キーは `version` なので**新規 DB は主キー衝突で
   マイグレートできず、しかも 2 本目（`node_system`）の DDL が一切入らない**
   （＝`node.system` 列が無い状態になり、#392 が静かに fail-open していた）。
   既存 DB は増分適用済みなので露見しなかった。#413 のテストを回す際に踏んだ。
   → 後からマージされた `node_system` を **0058 へリネーム**（既存 DB の checksum を壊さない
   唯一の向き）＋ `scripts/check-migration-versions.sh` を CI ゲートに追加して再発を止めた。
5. **フルプール再評価（#818・§8）の確認事項**:
   - F8＋F3 → F9① → F4 の優先順位（§5）で起票してよいか。
     F8 は「プール全体で 12.8 ev/s を超えると workflow のイベントトリガが際限なく遅れる」潜在不具合なので、
     F1 と同じく不具合として先行させたい。潜在不具合として **#847** に起票済み（着手順は確認待ち）。
   - F9②（一括取込の別キュー化か、テナント round-robin claim か）の方式。
   - §6 の再評価条件 2・3 の書き換え（プール総和で判定する・条件 3 を「プール分割の発火指標」に置き換える）への合意。

---

## 8. フルプール前提の再評価（2026-10-22・#818）

§3.3 の 4〜7 と §6 の 2・3 は、もともと「cell＝顧客ごとに 1 インスタンス」を前提に書いていた。
SaaS の既定は全テナント共有のフルプールなので（design §4.1・SAAS.5）、その前提で測り直し、書き直した。

### 8.1 プールで何が変わるか

- `storage_event_outbox`・`outbox_delivery`・`job_queue` は**全テナントで 1 表ずつ**ある
  （`tenant_id` 列で区別する。テナント別の索引は持たない）。
- コンシューマも全テナントで 1 本ずつになる。RAG relay は advisory lock のリーダー 1 本、
  workflow はリース取得者 1 本で動く。miniapp-functions と GC は全レプリカで動くが、
  処理する対象はやはりプール全体の 1 本のストリームである。
  したがって**コンシューマの処理能力の上限は、そのままプール全体の上限になる**。
- 1 社の負荷（一括取込など）が、同じ FIFO の中で他社の前に並ぶ。

### 8.2 プール全体のイベントレート見積り

outbox に 1 行を書く発生源をコードから洗い出し、ピーク時間帯のアクティブ利用者 100 人あたりで見積もる。
件数/人/時の値は仮定であり、アルファ後の実測で校正する。

| 発生源 | コード上の根拠 | 仮定 | ev/s（100 人あたり） |
|---|---|---|---:|
| 共同編集の保存 | `collab/src/saver.rs` `SAVE_IDLE=3s`・`SAVE_MAX=30s`（編集中の文書 1 つが 3〜30 秒に 1 件） | 同時に編集中 10%（10 文書）× 10 秒に 1 件 | 1.0 |
| 人手のファイル操作 | `StorageService` の各書込（upload/update/rename/move/delete/restore/folder）が `emit_on` を 1 件 | 20 件/人/時 | 0.56 |
| チャット/エージェントのワークスペース書込 | `chat/src/workspace.rs` の `write_file_at`・`append_file_at`（1 回で 1 件） | 2 run/人/時 × 10 書込 | 0.56 |
| 構造化データの FSM 遷移 | `data/src/transition.rs` | 10 件/人/時 | 0.28 |
| （将来）構造化データの行書込 | data-platform §3.6 ④・§9.1（行の作成・更新・削除ごとに outbox 1 行） | 30 件/人/時 | 0.83 |
| **計** | | | **現状 ~2.4／将来 ~3.2** |

1 社 100〜1,000 人なら毎秒 2〜30 件で、旧版の「1 cell＝1 社で毎秒数件オーダー」と同じ桁になる。
プールではこの総和を見る。

| プールの規模（ピーク時アクティブ） | 現状 | 将来（行書込イベント込み） |
|---|---:|---:|
| 50 社 × 50 人（2,500 人） | ~60 ev/s | ~80 ev/s |
| 300 社 × 100 人（3 万人）＝一次目標の中心（data-platform §12 第 1 段「数十〜数百社」） | ~720 ev/s | ~960 ev/s |
| 1,000 社 × 200 人（20 万人）＝第 1 段の上端 | ~4,800 ev/s | ~6,400 ev/s |

これとは別に**バースト**がある。オンボーディング時のファイル共有の一括移行は 1 社で 10⁵〜10⁶ イベントになり、
CSV 一括取込も（行書込イベントが入れば）同じ規模になる。
**一次目標の定常ピークは 10²〜10³ ev/s、千社規模で数千 ev/s** と見込む。
§6 の条件 2（数千 ev/s）には、第 1 段の上端で初めて近づく。

### 8.3 実測

再現スクリプトは `docs/events/bench/`、生の出力（EXPLAIN (ANALYZE, BUFFERS) を含む）は
`docs/events/bench/results-2026-10-22.txt` にある。
環境は docker の PostgreSQL 16.15・既定設定（`shared_buffers=128MB`・`work_mem=4MB`）・4 vCPU で、
CPU は他のビルドと共有していた（loadavg 4〜15）。`migrations/` を全適用した本番と同一のスキーマ・索引・
autovacuum 設定を使い、`node` 20 万行を LEFT JOIN の相手にした。イベントの 1 割は `payload.event_type` 付きにした。
**絶対値ではなく、N（outbox 行数）・T（テナント数）・R（レート）に対する増え方を見るための測定である。**

**(a) 走査長: claim 系はテナント数に依存せず、outbox 行数にだけ比例する**（各 5 回の中央値・ms）

| N | T | claim（relay） | claim_undelivered | GC 1 バッチ（1,000 行） | latest_event_id（イベントの無い社） | SSE 1 poll（活動中）〔走査窓〕 | **SSE 1 poll（休眠・カーソル 0）** | テナント消去（outbox） |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 万 | 1 | 1.1 | 7.0 | 24 | 1.7 | 0.16〔10 行〕 | 2.4 | —¹ |
| 1 万 | 100 | 1.2 | 11 | 26 | 1.7 | 0.7〔2,000 行〕 | 2.2 | 5.7 |
| 1 万 | 1,000 | 1.1 | 7.8 | 32 | 2.3 | 2.7〔1 万行〕 | 2.9 | 3.2 |
| 10 万 | 1,000 | 0.5 | 129 | 48 | 17 | 4.9〔2 万行〕 | 19 | 21 |
| 100 万 | 1 | 0.5 | 776 | 24 | 98 | 0.14〔10 行〕 | 145 | —¹ |
| 100 万 | 100 | 1.0 | 840 | 29 | 74 | 0.65〔2,000 行〕 | 133 | 656 |
| 100 万 | 1,000 | 0.9 | 758 | 27 | 247 | 5.2〔2 万行〕 | 144 | 168 |

¹ T=1 では「1 社の消去」が表全体の DELETE になる（100 万行で 24.1 秒）。比較の対象外とする。
300 万行の段は、docker 既定の `/dev/shm`（64MB）では並列クエリの共有メモリが確保できずに失敗した（`could not resize shared memory segment`）。このため測っていない。1 万〜100 万の 3 点で線形性は確認できている。

- **`claim`（relay・`processed_at` 部分索引）は N にも T にも依存しない**（~1 ms）。
- **`claim_undelivered` は N に比例し（1 万 7〜11 ms → 10 万 66〜129 ms → 100 万 0.76〜0.84 s。2 回の計測の幅）、T には依存しない**。
  §2.1 と同じ形で、プール化しても変わらない。計画は outbox の PK 全走査と、台帳の `event_id` 索引全走査
  （他コンシューマの行をフィルタで捨てる）の Merge Anti Join で、**台帳コンシューマが増えるほど 1 行あたりのコストも増える**。
- GC が健全なら N は「GC 周期分の窓」になる（R × 最大 60 秒。平均は R × 30 秒）。1 行あたり ~0.8 µs として、
  `claim_undelivered` に使う DB 時間はコンシューマ 1 つあたり
  毎秒 (R/64) × (R × 30) × 0.8 µs ≈ **R² × 0.38 µs**。R=100 で 4 ms/s、R=300 で 34 ms/s、
  **R=1,000 で ~0.38 s/s** になる。二乗で効くので、F8 で上限を外すなら F3 を同時に入れる必要がある（§5）。
- `latest_event_id` は、活動中のテナントなら PK の逆順走査ですぐ当たる（~0.1〜0.4 ms）。
  一方**イベントの無いテナントでは全走査になる**（100 万行で 74〜247 ms）。T が大きいほど遅いのは計画の選び方による。
  T=1,000 ではテナントあたり ~1,000 行と見積もって PK の逆順走査を選ぶので、休眠テナントでは
  100 万行を索引順にヒープを引きながら読み切ることになる（`Rows Removed by Filter: 1000000`）。
  プールには休眠テナントが多いので、SSE の接続ごとにこれを踏む。F4 の `(tenant_id, id)` 索引で解消する。
- **SSE の 1 poll の走査長は、活動中のテナントでもプールのテナント数に比例し、休眠テナントでは N に比例する**。
  カーソルは、そのテナントの `event_type` 付きイベントが返ったときにしか進まない（`app-gateway/src/routes/events.rs:98-101`）。
  - 活動中のテナントでは、カーソルは自社の直前のイベントに止まっている。このため 1 poll は、
    自社のイベント同士の間に挟まった**他社の書込も含めて**読む。表の「活動中」は、カーソルを自社の 1 つ前の
    `event_type` イベントに置いた状態で、走査窓は 10 行（T=1）→ 2,000 行（T=100）→ 2 万行（T=1,000）、
    時間は 0.15 ms → 0.7 ms → ~5 ms だった。N には依らない。
    ベンチは書込を全テナントに均等に配っているので、走査窓は ~10 × T〜20 × T 行になる。
    実運用では「自社の SSE 対象イベントの間隔のあいだに、プール全体で書かれた行数」がこれにあたる。
  - 休眠テナントでは、カーソルは接続時の値（`latest_event_id`。イベントが無ければ 0）から動かない。
    このため `id > cursor AND tenant_id = $1` が**残っている outbox 全体を毎秒走査する**
    （1 万 2.4 ms → 10 万 19 ms → 100 万 ~140 ms）。GC が健全なら N は GC 窓（R × 最大 60 秒）に収まるが、
    R=1,000 ev/s なら 6 万行になり、休眠接続 1 本あたり毎秒 ~10 ms かかる。
  - どちらも接続数倍になる（§2.3）。F4 の `(tenant_id, id)` 索引で、自社の行だけの範囲走査に変わる。
- **テナント消去は Postgres なら単純な `DELETE` で済む**。100 万行の中から 1 社分を消して 0.17〜0.66 秒。
  索引が無いので全走査になるが、これは管理操作なので許容でき、F4 の索引でさらに軽くなる。
- 容量は outbox 本体＋索引で ~300 B/イベント、2 台帳を足して ~600 B/イベント。
  R=1,000 で 60 秒窓を持っても ~36 MB にしかならない。

**(b) job_queue: 1 社の一括取込が、他キューと他社を巻き込む**（rag_ingest に B 件・その 99% が 1 社）

| B | rag_ingest claim | **chat_generation claim**（別キュー） | `delete_tenant`（小さい社） | 小さい社の前に並ぶ件数 |
|---:|---:|---:|---:|---:|
| 1,000 | 0.4 ms | 0.6 ms | 0.3 ms | 990 |
| 10 万 | 0.4 ms | **13 ms** | 9.7 ms | 99,000 |
| 100 万 | 0.3 ms | **285 ms** | 129 ms | **990,000** |

- `jobq::claim` は `ORDER BY id` で PK を走査し、`queue` と `visible_at` をフィルタで判定する
  （`job_queue_claim_idx (queue, visible_at)` は使われない）。このため、**先に積まれた別キューの大量バックログを
  毎回読み飛ばす**（100 万件で `Rows Removed by Filter: 1000000`）。対話系の chat_generation が、
  1 社の RAG 一括取込のせいで claim ごとに ~0.3 秒遅れる。F9① で解消する。
- 同じ rag_ingest キューの中では、小さい社のジョブが 1 社の取込の後ろで FIFO に待つ。待ち時間は
  ingest の処理速度で決まる（`consumer_concurrency=2`・1 件 1 秒なら 99 万件で ~5.7 日）。これはイベント経路ではなく
  キューの公平性の問題で、ブローカに替えても解けない（F9②）。

**(c) 書込側**: outbox の INSERT だけなら 4 クライアントで 1,400〜2,600 ev/s。
WAL/イベントは 2 回の計測で **489 B（T=1,000）と 10,964 B（T=1）** と大きく割れた。
どちらも `pg_current_wal_lsn()`（クラスタ全体）の差分なので、同居 DB の書込を含む上限値である。
差の理由は切り分けていない。候補は次の 2 つ。
- 同じサーバ上の他 DB（並行して走っていた別のテスト）の WAL
- 直前の一括ロード（node 20 万行・TRUNCATE）でチェックポイントが走った直後の full-page write
  （チェックポイント後に初めて触るページは 8 KB 丸ごと WAL に載る）

行サイズ（~300 B）と索引 1 本から見て、定常時の本来の値は下限側の ~0.5 KB に近いと見ている。
ただし、§6 条件 3 の「WAL の過半」判定に使う前に、本番機材で `pg_stat_wal`（または `pg_walinspect` で DB 別に）測り直すこと。

**(d) 混合負荷: Postgres は追従する。追従できないのはコードの周期である**（T=1,000・書込と全コンシューマを同時に回す）

3 行とも 2026-10-22 11:44Z の再計測値である。relay のレイテンシは、relay txn 内で INSERT した時点の `clock_timestamp()`
（`payload.relayed_at`）から outbox の `created_at` を引いて求めた。初回の計測は `job_queue.enqueued_at`
（`DEFAULT now()`＝relay txn の開始時刻）を使っていたが、これは claim と INSERT の時間を含まない。
また、BEGIN の後・claim の前にコミットされたイベントでは負の値になり、relay のパーセンタイルを過小に見せていた。
`created_at` は書込 txn の開始時刻なので、どのレイテンシにも書込 txn 自身の時間が含まれる。

| 条件 | 達成した書込レート | relay（書込→job_queue） | workflow 台帳 | miniapp-functions 台帳 | 終了時の未配送（workflow / miniapp） | 終了時の outbox 残存 |
|---|---:|---|---|---|---:|---:|
| 待ち無し（DB の上限）・目標 1,000 ev/s・60 秒 | 498 ev/s | p50 11 ms / p99 37 ms | p50 10 ms / p99 35 ms | p50 10 ms / p99 37 ms | 0 / 31 | 420 |
| 待ち無し（DB の上限）・目標 3,000 ev/s・60 秒 | **1,086 ev/s** | p50 13 ms / p99 62 ms | p50 18 ms / p99 65 ms | p50 18 ms / p99 72 ms | 22 / 22 | 264 |
| **本番コードの周期**・100 ev/s・120 秒 | 99 ev/s | **p50 1.1 s / p99 4.8 s**（relay 未処理残 292） | **p50 75 s / max 110 s** | **p50 17 s / max 105 s** | **10,895 / 10,769** | **10,959** |

- 待ち無しで回すと、書込 ~1,100 ev/s でも 3 本のコンシューマと GC は追従し、未配送は 1 バッチ分以下に収まった（relay 未処理残は 59 件）。
  この機材では**先に頭打ちになったのは書込側（同居負荷下の INSERT）**で、Postgres のイベント経路の上限には届いていない。
  relay は本番と違い 1 文にまとめているので（本番は enqueue が 64 往復）、ここで得た値は DB 側の上限にあたる。
- **注: 待ち無しの行は「claim と配送記録だけ」の DB 単体の上限である**。
  台帳 1 本の claim＋配送記録は 1 txn ~6 ms（目標 1,000 ev/s の行で 171 tps）だった。
  満杯のバッチで回し続けたと仮定すると、171 tps × 64 件で ~1.1 万 ev/s 相当になる。
  ただし、計測中のバッチは満杯ではなかった。満杯なら 1 txn はもっと重いので、これは楽観側の目安にすぎない。
  本番の workflow は 1 イベントごとに `match_event`（トリガ照合・run 起動）と `wake_event_waits` を直列に呼ぶ
  （`api/src/workflow_runtime/mod.rs:345-356`）。miniapp-functions も 1 イベントごとに購読照会と越境判定を行い、
  commit 後に関数起動を直列に回す。このため F8 後の実効上限はこれより低い。
  §6 条件 2 の基準には、この値ではなく、本番のコンシューマ処理を含めて測った値を使う。
- **本番コードの周期では、100 ev/s で workflow と miniapp-functions が追従できない**。workflow は 0.13 tps × 64 ≈ 9 ev/s、
  miniapp-functions は 0.20 tps × 50 ≈ 10 ev/s しか捌けず、未配送は ~90 ev/s の速さで線形に伸びた。
  `pgbench -R` はポアソン到着で、120 秒では 16〜24 回しか回らないので、tps には揺れがある。理論上限は 12.8 / 10 ev/s。
- **RAG relay も、本番の周期では余裕が無い**。上限 ~128 ev/s に対して 100 ev/s は利用率 ~78% にあたる。
  到着の揺れで待ち行列ができ、書込→job_queue は p50 1.1 s / p99 4.8 s、終了時に 292 件が未処理で残った。
  初回計測の p50 0.6 s / p99 2.7 s は、上に書いた測り方の誤りで過小だった。この結果は F8① の根拠を強める。
  GC は両方に配送されるまで消せないので outbox も同じ速さで伸び、最大遅延は計測時間とともに伸び続けた（上限が無い）。
  **プールの一次目標（10²〜10³ ev/s）では、これが定常状態になる。**

### 8.4 コードの定数が決める上限

| コンシューマ | 定数（所在） | 実行単位 | 上限 |
|---|---|---|---:|
| RAG relay | `RELAY_BATCH=64`（`rag/src/pipeline/relay.rs:15`）・満杯でも毎回 `relay_poll_ms=500` 待つ（`rag/src/pipeline/mod.rs:100`） | プールに 1 本（advisory lock のリーダー） | ~128 ev/s |
| workflow | `claim_undelivered(.., 64)`（`api/src/workflow_runtime/mod.rs:320`）・`tick_secs=5`（同 :81）で、タイマー起床などと直列 | プールに 1 本（リース取得者） | **12.8 ev/s** |
| miniapp-functions | `claim_undelivered(.., 50)`・5 秒 sleep（`api/src/miniapp_triggers.rs:47,71`） | レプリカごと | 10 ev/s × レプリカ数 |
| outbox GC | 1,000 行 × 20 バッチ / 60 秒（`storage/src/outbox_gc.rs:30,36`・`event/gc.rs:18`） | レプリカごと | 333 ev/s × レプリカ数 |
| （参考）RAG ingest consumer | `consumer_concurrency=2`（`rag/src/config.rs:165`）・満杯なら待たずに続ける。処理時間（パース＋埋め込み）で律速 | プールに 1 本（relay と同じリーダー） | 2 件 ÷ 1 件あたりの処理秒（1 秒なら **2 ev/s**） |

GC は relay の ack（`processed_at`）と、登録済み全台帳コンシューマへの配送の両方が揃った行しか消さない。
このため**プール全体で持続できるイベント経路のレートは、上 4 行の最小値（現状 ~10〜12.8 ev/s）**になる。
RAG ingest consumer は GC を止めない（relay が `processed_at` を立てた時点で outbox 側は消せる）。
ただし、その上限を超えた分は job_queue の rag_ingest に溜まる。その結果、索引の反映が遅れ、
§8.3(b) のキュー間干渉の原因にもなる。これは §8.2 の一次目標の 1〜2 桁下で、1,000 人規模の 1 社のピークにも届かない。
上限は Postgres の能力ではなく `sleep` と `LIMIT` の定数から来ているので、配送基盤をブローカに替えても同じループなら同じ上限になる。
これが F8 である。

### 8.5 NFR-10（テナント消去）との衝突は、プールで強まる

§3.3-5 に書いたとおり。要点だけまとめる。

- **Postgres**: outbox・台帳・ジョブは `DELETE ... WHERE tenant_id` で消え、消去対象は `pg_catalog` から
  機械的に導出され（`storage::tenant_scope`）、ほかの全テーブルと 1 トランザクションで消える。
  実測は 100 万行中の 1 社分で 0.17〜0.66 秒（§8.3）。期限付き消滅証明は PG バックアップの保持期限で閉じる。
- **共有の追記ログ（Iggy/Kafka）**: 1 テナント分を選んで消す直接の操作が無い。近似する手段は crypto-shredding
  （テナント鍵の破棄）と Kafka の compaction＋tombstone で、どちらも鍵管理か compaction 完了の証明を別に持つ。
  近似手段を使わずに retention を短くすると、Iggy 固有の唯一の利点だった replay（§2.6）が失われる。
  テナント単位に stream を割ると NFR-8 に反する。cell 前提では「その社の stream を捨てる」で済んでいたが、
  プールではこの逃げ道が無い。
- **NATS JetStream**: subject フィルタ付き purge とメッセージ単位の消去があるので、消すこと自体はできる。
  ただし、別の消去経路とその網羅性検査、PG とトランザクションをまたぐ中間状態、ブローカのバックアップという
  3 つの消去対象が増える。

### 8.6 結論

- **Postgres 継続。ブローカ（Iggy・NATS を含む）は導入しない。** プール前提で見ると、
  NFR-9/10 との衝突（§3.3-4・5）は強まり、部品点数（6）は SaaS では弱まるがオンプレと二実装保守に移るだけで、
  スループット（7）はプール総和でも Postgres に収まる。§2 の課題にもプールで新たに見えた課題にも、
  **ブローカでしか解けないものは無い**。2.2（消費経路の二重化）と 2.6（replay）にはブローカが効くが（§3.2）、
  Postgres 内でも F2・F6 で同等または近似できる。
- **プールで先に詰まるのは「コンシューマの固定周期」と「テナント間・キュー間の干渉」**で、どちらも Postgres のまま直す。
  優先順位は **F8＋F3（同時）→ F9① → F4 → F2 → F5 → F6 → F7**（§5）。
- ブローカを再検討するのは §6 の条件が成立したときだけとする。とくに条件 2 は、F8 の後に本番機材で
  §8.3 の mixed を回して上限を測り直し、その 50% をプールのピークが継続して超えたときである。
  その場合も先に検討するのは**プール分割**（data-platform §12 第 2 段。イベント経路もテナント単位で割れる）で、
  第一候補は引き続き NATS JetStream（§7）とする。

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

### §8.3（フルプール・2026-10）の再現手順

§2.1 の手順を、本番と同一のスキーマ（`migrations/` 全適用）とテナント数・レートの軸に広げたもの。
スクリプトは `docs/events/bench/` に置いた。生の出力は `results-2026-10-22.txt` にある。

```bash
# 専用 DB を作り migrations を全適用する（スクリプトは表を TRUNCATE するので共有 DB に向けないこと。接続先の current_database() が ^shiki_[0-9]+$ でなければ停止する）
docker exec shiki-postgres-1 psql -U postgres -c 'create database shiki_818'
export PGURL=postgres://postgres:postgres@localhost:5432/shiki_818
for f in migrations/*.sql; do psql -q -v ON_ERROR_STOP=1 "$PGURL" -f "$f"; done

bash docs/events/bench/outbox-pool-bench.sh scan    # §8.3(a) N × T の走査長（EXPLAIN 5 回の中央値）
bash docs/events/bench/outbox-pool-bench.sh jobs    # §8.3(b) 一括取込バックログ下の jobq claim
bash docs/events/bench/outbox-pool-bench.sh emit    # §8.3(c) 書込側の上限と WAL/イベント
CONSUMERS=db   bash docs/events/bench/outbox-pool-bench.sh mixed 3000 60   # §8.3(d) DB の上限
CONSUMERS=code bash docs/events/bench/outbox-pool-bench.sh mixed 100 120   # §8.3(d) 本番コードの周期
# まとめて: bash docs/events/bench/outbox-pool-bench.sh all
```

- `scan` は outbox に N 行を積み、テール 64 行を除いて全行を `processed_at` 済み＋2 台帳（`workflow`・`miniapp-functions`）配送済みにする。
  これは GC が止まって溜まった状態にあたる。N を GC 窓の大きさと読めば、GC が回っている定常状態にもあたる。
  そのうえで本番と同一形状のクエリを `BEGIN; EXPLAIN (ANALYZE, BUFFERS) ...; ROLLBACK` で測る。
- `mixed` は pgbench で書込（`emit.pgbench.sql`）・relay（`relay.pgbench.sql`）・台帳 2 本（`ledger.pgbench.sql`）・
  GC（`gc.pgbench.sql`）を同時に回し、書込→消費のレイテンシと終了時の滞留を出す。
  `CONSUMERS=code` は本番の周期を `pgbench -R`（ポアソン到着・平均周期が一致）で再現する。
- 本番機材で測り直すときは、少なくとも `mixed` を `CONSUMERS=db` でレートを上げながら回し、
  未配送が伸び始めるレートを §6 の条件 2 の基準にする。
