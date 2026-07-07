# Phase 10 — ワークフロー基盤（workflow-engine・shiki script・skill・secrets）

> 目的: [miniapp-platform.md](../miniapp-platform.md) の概念スタックを実装する。
> **workflow-engine（自作 Durable Execution）を唯一の長時間実行ランタイム**として立ち上げ、
> shiki script（script-runtime）・skill＆スキルストア・シークレット管理を載せる。
> 完了の定義(DoD): ユーザーが dnd／AI（チャット）でワークフローを作成し、スケジュール・イベント・対話の
> 3種トリガで実行できる。スケジュール/イベント実行は有効化時の明示委譲で権限が付与され、委譲者失権で
> fail-closed 停止する。script ノード・skill ノード・agent.invoke ノード（サンドボックス設定パネル付き）・
> http.request ノード（シークレット宛先束縛）が動き、ステップリトライ・fan-out・concurrency・rate limit・
> 実行履歴 UI（OTel/Langfuse 突合）が機能する。
>
> ⚠️ **着手前に [設計上の落とし穴](../design-caveats.md) の PIT-31（at-least-once 副作用）・PIT-34（委譲失効の検知）・
> PIT-35（script ホスト関数ブリッジ）・PIT-36（シークレット宛先束縛の迂回）を確認すること。**
>
> 📘 **詳細設計は [docs/workflow/](../workflow/README.md) が正本**（2026-07 フェーズ先取りで確定・issue #119）:
> [ir.md](../workflow/ir.md)（IR 仕様・Task 10.1）／[engine.md](../workflow/engine.md)（実行エンジン・Task 10.2〜10.6/10.14）／
> [script.md](../workflow/script.md)（shiki script 処理系・Task 10.7/10.8）。Task ↔ 章の対応表は
> [README §5](../workflow/README.md) を参照。

## 部分前倒し（Stage A / Stage B・2026-07-07 決定・issue #121）

> **human 決定（2026-07-07）**: Phase 10 のエンジン核心に**部分前倒しで着手**する。詳細設計（docs/workflow/・#119）が
> 完了しており、依存分析の結果、本フェーズの本質的ブロッカーは Phase 9 全体ではなく **6.1（artifact 共通枠）と
> 9.x の一部（data ノード・skill レジストリ・同意 UI パターン）のみ**。エンジン核心は既存実装
> （authz/storage/rag/jobq/outbox/chat run(3.11)/sandbox(Phase 4)/llm-gateway）で依存が満たせる。
>
> - **Stage A（前倒し・着手可能）**: 10.0・10.1a・10.2・10.3・10.4a・10.5・10.6a・10.7・10.8・10.9・10.10。
>   **前提**として ①[Phase 6 Task 6.1（artifact 共通枠）を先行実施**する（依存 3.7 は充足済み。
>   暫定テーブルで作って後で移行する二重実装を避ける）②**outbox の per-consumer fan-out 化**（下記 P10-A0）。
> - **Stage B（Phase 6/9 の該当タスク合流後）**: 10.1b・10.4b・10.6b・10.11・10.12・10.13・10.14・10.15。
> - **Stage A の DoD**: API 経由で IR を保存（**V1/V2/V3/V5/V6/V7 検証付き**。V4 のうち **secret 照合は 10.9 完了後に
>   有効化**、**skill 照合は Stage B（10.1b）**）でき、schedule／イベント（storage.write）／対話（API 起動）の 3 種トリガで run が実行され、
>   script・制御・storage/rag・AI 2 種・http.request・script→workflow.start の各ノードがステップリトライ・冪等キー・
>   委譲チェック（run 開始時＋棚卸し）付きで動き、run/step が監査・OTel に乗る。**UI（dnd・実行履歴）・skill・
>   data 系ノードは含まない**（Stage B）。V3（ワークフロー語彙の閉じた集合照合＝ハルシネーション境界）は
>   Stage A に必須で含む（10.1a）。
> - 実行順の目安: **P10-A0（outbox fan-out）** ∥ 10.0 ∥ 10.7 ∥ 10.9 ∥ 6.1 → 10.1a → 10.2 → {10.3, 10.4a, 10.5}
>   → {10.6a, 10.8, 10.10}。

### Task P10-A0: outbox の per-consumer fan-out 化（Stage A 前提・新設）
- **area**: data / **path**: `crates/storage`（event）, `crates/rag`（relay）, migrations
- **問題**: 現状の `storage_event_outbox` は単一 `processed_at` ack で、`crates/rag/src/pipeline/relay.rs` が
  `claim`（`processed_at IS NULL` を SKIP LOCKED）→ enqueue → `mark_processed` と**破壊的に消費**している。
  workflow-engine が storage.write の 2 人目のコンシューマになると、**RAG が先に処理済みにした瞬間に
  ワークフローがイベントを取りこぼす**（逆順なら RAG が取りこぼす）。design §4.3 は outbox を「fan-out 点」と
  謳うが、実装は単一コンシューマのまま。イベントトリガ（10.3）着手前にここを是正する。
- **仕様**: per-consumer カーソル方式へ移行する。案: `outbox_consumer(tenant_id, consumer, last_seq)` を置き、
  各リレー（rag / workflow）が自分のカーソルから未処理を読み、消費後に自分のカーソルだけを進める
  （outbox 行はすべてのコンシューマが通過した後に GC）。既存 RAG relay を新方式へ載せ替え、挙動不変を担保する。
- **受け入れ条件**:
  - [ ] 同一 storage 書込イベントが rag と workflow の**両方**に届く（片方の消費が他方を消さない）
  - [ ] 既存 RAG インジェストの挙動・テストが不変（純移行）
  - [ ] コンシューマ追加が outbox 生成側の変更なしに行える（fan-out 点として機能）

| ID | タイトル | area | 依存 | Stage |
|----|---------|------|------|-------|
| P10-A0 | outbox の per-consumer fan-out 化（storage.write を rag と workflow の 2 消費者へ） | data | 3.11（済） | **A**（10.3 の前提） |
| 10.0 | durable 共有基盤の切り出し（chat 3.11 の claim/リース/fencing/seq を共通クレート化） | data | 3.11（済） | **A** |
| 10.1 | ワークフロー IR スキーマ＋artifact 化＋語彙照合検証 | data | 6.1（前倒し）／9.1・9.13 は 10.1b | **A**（10.1a）＋B（10.1b） |
| 10.2 | run/step 永続化＋ワーカー（claim/リース/チェックポイント） | data | 10.0, 10.1a | **A** |
| 10.3 | トリガ: スケジューラ（cron・リーダーリース）＋イベントマッチング（outbox） | data | 10.2, **P10-A0** | **A**＋B（event source: A=storage.write / B=data 系・9.10 後） |
| 10.4 | 実行主体・委譲モデル（workflow プリンシパル・同意フロー・fail-closed 停止） | auth | 10.2／9.13 は 10.4b | **A**（10.4a）＋B（10.4b） |
| 10.5 | 制御ノード（分岐/並列/join/待機）＋ステップリトライ＋concurrency/rate limit | data | 10.2 | **A** |
| 10.6 | 能力ノード（storage/data/rag/notify）＋AI ノード2種＋ノード設定パネル契約 | agent | 10.2, 5.1※／9.8・9.10 は 10.6b | **A**（10.6a）＋B（10.6b） |
| 10.7 | script-runtime（swc＋wasmtime/QuickJS・Shiki.* ブリッジ・非特権プロセス） | sandbox | – | **A**＋B（`Shiki.data.*`/`notify` の能力面のみ 9.2/9.10 後） |
| 10.8 | script ノード＋script→ワークフロー起動 API | data | 10.7, 10.2 | **A** |
| 10.9 | シークレット管理（crates/secrets・KeyProvider・宛先束縛・監査） | auth | – | **A** |
| 10.10 | http.request ノード（egress allowlist × シークレット宛先束縛） | data | 10.9, 10.5 | **A** |
| 10.11 | skill artifact＋スキルストア（レジストリ再利用）＋skill ノード/エージェントマウント | app | 9.13, 10.7 | B |
| 10.12 | dnd ワークフローエディタ（IR 直接編集・ノード設定右パネル） | frontend | 10.1 | B |
| 10.13 | AI 編集（チャット→IR 生成/変更・検証済みスペック・dnd に引き継ぎ） | ai | 10.12, 6.3 | B |
| 10.14 | 実行履歴 UI＋Observability（run/step・OTel span・Langfuse 突合） | obs | 10.2, 3.8 | B（OTel/監査計装自体は Stage A 各タスクに含む） |
| 10.15 | first-party skill 初期セット（Slack 通知等・http.request ラップ） | app | 10.10, 10.11 | B |

※ 10.6a の `agent.invoke` は Phase 4 完成済みの wasm ティアで実行する。5.1（自律エージェント）が未完の間は
チャット相当の制約ツールセットで先行し、5.1 合流時にフルツール構成を解禁する。

---

## 詳細

### Task 10.0: durable 共有基盤の切り出し（Stage A・新設）
- **area**: data / **path**: `crates/durable`（名称は着手時確定・[engine.md §1](../workflow/engine.md) の提案）, `crates/chat`
- **仕様**: chat 3.11（#82）で実装済みの claim（`FOR UPDATE SKIP LOCKED`）・リース＋heartbeat・
  **fencing token**・`(id, seq)` 追記 exactly-once・Redis pub/sub 配信を共通クレートへ切り出し、
  chat を移行する。キュー・レーン・優先度・状態機械は共有しない（[engine.md §1.2](../workflow/engine.md) の分担表が正）。
- **受け入れ条件**:
  - [ ] chat の既存テスト（claim/リース/fencing/seq）が共通クレート経由で全緑のまま
  - [ ] chat の挙動・レーン分離に変化がない（純リファクタ）
  - [ ] workflow-engine が同一プリミティブを import できる

### Task 10.1: ワークフロー IR スキーマ＋artifact 化＋語彙照合検証
> **Stage 分割**: **10.1a（Stage A）** = IR スキーマ・保存時検証 **V1/V2/V3/V5/V6/V7**・ワークフロー語彙
> （ノード type・スコープ・ツール名・モデル名・イベント source）の codegen 単一定義の先行整備・artifact 化
> （6.1 前倒し枠）・V4 の secret 照合（10.9 後に有効化）。
> **V3（閉じた語彙集合への照合＝ハルシネーション境界）は Stage A に必須で含む**（AI 生成 IR が実在しない
> スコープ/ツール/モデルを参照するのを保存時に拒否する境界であり、これを Stage B に送ると Stage A の IR 保存経路が
> 未防御になる）。**10.1b（Stage B）** = V4 の skill 照合・9.1 マニフェスト/9.13 レジストリ統合。
- **area**: data / **path**: `crates/workflow-engine`, `crates/app-platform`
- **仕様**: IR（JSON DAG: ノード種・接続・パラメータ・トリガ・リトライポリシ）のスキーマ定義。
  バージョン付き artifact（6.1 枠・ReBAC 共有）。保存時にスキーマ検証＋**codegen 認可語彙・skill/secret レジストリの
  閉じた集合への照合**（実在しないツール/スコープ/skill/secret 参照を拒否＝ハルシネーション境界）。
- **受け入れ条件**:
  - [ ] IR を artifact として保存・バージョン管理・ReBAC 共有できる
  - [ ] 存在しないツール名/スコープ/skill/secret を参照する IR が保存時に拒否される
  - [ ] 旧バージョンの IR が不変で取得できる

### Task 10.2: run/step 永続化＋ワーカー
- **area**: data / **path**: `crates/workflow-engine`, migrations
- **仕様**: `workflow_run`/`step_execution`（全行 `tenant_id` 必須・複合キー規約は #91 踏襲）。
  ワーカーは `FOR UPDATE SKIP LOCKED` claim＋リース（heartbeat）。ノード完了ごとにチェックポイント。
  リース失効→別ワーカーが完了済みステップを復元し未完ステップのみ再実行（at-least-once）。
  チャット run（3.11/#82）と claim/リース/seq イベントの**共有モジュール**を切り出す（キューは分離）。
- **受け入れ条件**:
  - [ ] ワーカー kill →別ワーカーが完了済みステップを再実行せずに run を継続する
  - [ ] `(run_id, seq)` unique で追記が exactly-once に潰れる
  - [ ] 全テーブル・全クエリが tenant_id スコープ

### Task 10.3: トリガ（スケジューラ＋イベント）
> **Stage A 注記**: イベント source は既存 outbox で発行済みの `storage.write` 系のみ先行。
> `data.record.*`／`data.transition` source は 9.10（FSM ガード・outbox 発行）合流後に追加する。
- **area**: data / **path**: `crates/workflow-engine`
- **仕様**: cron 式を Postgres 保持・**リーダーリース付き単一スケジューラループ**が due run を enqueue（多重発火防止）。
  **発火の冪等化**: occurrence を `(workflow_id, scheduled_at)` unique でトランザクショナルに記録してから enqueue
  （リーダーが enqueue 直後にクラッシュ→再起動しても同一 occurrence を二重投入しない・PIT-31 参照）。
  イベントトリガは既存 outbox（storage 書込・record 変更・status 遷移）とトリガテーブルのマッチング。
  **前提**: storage.write を購読するには outbox が per-consumer fan-out 化されていること（P10-A0）。現状の
  単一 `processed_at` 破壊消費のままだと rag と取り合ってイベントを取りこぼす。
- **受け入れ条件**:
  - [ ] 複数インスタンス起動時もスケジュールが1回だけ発火する
  - [ ] スケジューラを enqueue 直後に kill →再起動しても同一 occurrence の run が1つしか作られない
  - [ ] storage 書込でワークフローが起動する（Stage A・**rag の消費と取りこぼしなく両立**する＝P10-A0 前提）
  - [ ] record 変更／status 遷移でワークフローが起動する（**Stage B**・9.10 の outbox 発行後。Stage A の実装は source を閉じた集合で持ち、追加が既存経路の再設計にならない形にする）
  - [ ] 無効化済みワークフローのトリガが発火しない

### Task 10.4: 実行主体・委譲モデル（FR-12 最重要）
> **Stage 分割**: **10.4a（Stage A）** = workflow プリンシパル（`Namespace::workflow()`）・**`AuthContext` の
> principal 種別拡張**（下記）・run 開始時の委譲有効性チェック・棚卸しジョブ・委譲の付与/失効 API（管理者向け・最小 UI）。
> **10.4b（Stage B）** = 9.13 の同意インストールパターンとの UI 統合・管理ダッシュボード棚卸し画面（12.3 接続）。
>
> ⚠️ **AuthContext の principal 種別は 10.4a の必須成果物**: 現行の `AuthContext::subject()` は常に
> `user:<tenant>|<id>` を構築するため、`Namespace::workflow()` ビルダ追加だけでは schedule/event run の能力呼び出しが
> workflow サブジェクトで check されず、**委譲タプルが一切照合されない**（委譲者の user 権限で動くか、認可が通らない）。
> principal に種別（user / workflow）を導入し、schedule/event run の全 check・ListObjects が `workflow:<tenant>|<id>`
> で評価されるよう `crates/authz` を拡張する（engine.md §6.1）。これがないと Stage A の委譲 storage/rag/http 経路が
> 正しく認可されない。
- **area**: auth / **path**: `crates/workflow-engine`, `crates/authz`
- **仕様**: `authz::Namespace` に `workflow()` ビルダ追加（`workflow:<tenant>|<id>`）。
  対話トリガ=本人 ReBAC ∩ 宣言スコープ ∩ ノード設定。スケジュール/イベント=専用プリンシパルへ
  **有効化時に有効化者が自分の権限範囲内から明示委譲**（同意 UI・9.13 パターン再利用）。
  委譲タプルは委譲者にリンクし、**委譲者の失権・退職で該当ワークフローを fail-closed 停止→再同意要求**。
  監査: run_id・トリガ種別・実行プリンシパル・委譲者。
- **受け入れ条件**:
  - [ ] schedule/event run の能力呼び出しが `workflow:<tenant>|<id>` サブジェクトで check される（user サブジェクトに落ちない・principal 種別拡張が効く）
  - [ ] 対話トリガで本人が読めないデータにワークフロー越しでも到達できない
  - [ ] 有効化者の権限外スコープの委譲が拒否される
  - [ ] 委譲者の権限剥奪後、次回実行が開始されず「再同意要求」状態になる（黙って動き続けない）
  - [ ] ノード設定はどう書いても実効権限を拡大できない（縮小のみ）

### Task 10.5: 制御ノード＋リトライ＋concurrency/rate limit
- **area**: data / **path**: `crates/workflow-engine`
- **仕様**: 分岐・並列（fan-out）・join・待機（時間/イベント）。step retry policy（max/backoff・冪等キー供給）。
  同時実行上限（テナント/ワークフロー/ノード種の3階層・Postgres カウンタ）。rate limit（テナント×能力トークンバケット・Redis）。
- **受け入れ条件**:
  - [ ] fan-out→join が正しく待ち合わせ、失敗分岐のみリトライされる
  - [ ] 上限超過の run/step が実行されず順番待ちになる（拒否ではなくバックプレッシャ）
  - [ ] rate limit 超過が step を失敗させず遅延させる

### Task 10.6: 能力ノード＋AI ノード2種＋ノード設定パネル契約
> **Stage 分割**: **10.6a（Stage A）** = storage.read/write/list・rag.search・`llm.invoke`・`agent.invoke`
> （Phase 4 の wasm ティア・5.1 未完の間は制約ツールセット）。**10.6b（Stage B）** = data.query/data.record.*/
> data.transition（9.2/9.3/9.10 後）・notify.send（通知基盤）・ノード設定パネルの TS codegen 契約（10.12 と対）。
- **area**: agent / **path**: `crates/workflow-engine`, `crates/agent-core`
- **仕様**: storage/data/rag/notify ノード（実行主体 AuthContext で既存チョークポイント経由）。
  `agent.invoke` ノード=サンドボックス起動（ノード設定: egress allowlist・マウントスコープ・許可ツール・モデル・上限。
  **設定は capability 縮小のみ**）。`llm.invoke` ノード=llm-gateway 直行（モデルカタログ・予算）。
  ノード設定スキーマは codegen で TS 型へ（右パネル UI の契約）。
- **受け入れ条件**:
  - [ ] agent.invoke がノード設定どおりに制限されたサンドボックスで実行される
  - [ ] ノード設定で ReBAC 外の権限が付与できないことがテストで担保される
  - [ ] 全ノードの呼び出しが監査に run_id 付きで残る

### Task 10.7: script-runtime
> **Stage A 注記**: ランタイム・ブリッジ・リソース制限は全部 Stage A。ただし `Shiki.*` の能力面は
> 既存チョークポイントがある storage/rag（＋http/workflow.start）で先行し、**`Shiki.data.*`・`Shiki.notify.send` は
> 9.2/9.10 合流後（Stage B）に追加**する。下記受け入れ条件の data.query は Stage A では storage.read 等で読み替える
> （検証対象は「同期スタイル＋通常認可・監査」の経路であり API の種類ではない）。
- **area**: sandbox / **path**: `crates/script-runtime`
- **仕様**: swc で TS→JS、**wasmtime 上の QuickJS**（javy 方式）で実行。専用**非特権プロセス**・RPC・インスタンス使い捨て。
  fuel/メモリ上限/epoch interruption。`Shiki.*` ホスト関数ブリッジ（同期スタイル→ホスト側 async 橋渡し）は
  能力ゲートウェイへ合流し AuthContext 認可・監査を通る。npm import 不可。
- **受け入れ条件**:
  - [ ] `Shiki.storage.read(...)`（Stage B では `Shiki.data.query(...)`）の同期スタイル script が実行でき、認可・監査が通常経路で効く
  - [ ] 無限ループ/メモリ爆発が fuel/上限で強制中断される
  - [ ] wasm 内からホスト関数以外の外界（fs/net）に到達できない
  - [ ] コールドスタートが ms 級（スプレッドシート関数要件）

### Task 10.8: script ノード＋script→ワークフロー起動
- **area**: data / **path**: `crates/workflow-engine`, `crates/script-runtime`
- **仕様**: script ノード=1 回の有界実行（タイムアウト・ステートレス・at-least-once）。
  `Shiki.workflow.start(name, input)`（fire-and-forget / run_id 取得）。script 自体は durable にならない。
- **受け入れ条件**:
  - [ ] script ノードのリトライが冪等キー供給付きで再実行される
  - [ ] script から名前指定でワークフローを起動でき、権限は実行主体で評価される

### Task 10.9: シークレット管理
- **area**: auth / **path**: `crates/secrets`, migrations
- **仕様**: write-only/use-only（平文読み返し API なし）。`secret:<tenant>|<id>` ReBAC（owner/can_use・同意フロー付与）。
  **宛先束縛**（登録時宣言ホスト・実行時 fail-closed 強制・リダイレクト先も再検証）。
  envelope encryption＋ **`KeyProvider` トレイト**（Cloud KMS / ローカルキーファイル）。解決イベント毎回監査。
  ログ・run 履歴・エラーの自動レダクト。
- **受け入れ条件**:
  - [ ] 登録後、いかなる API/UI からも平文を読み返せない
  - [ ] can_use を持たない実行主体の解決が拒否される
  - [ ] 宣言宛先以外への添付（リダイレクト含む）が拒否され監査に残る
  - [ ] run 履歴・ログに平文が現れない（レダクトテスト）

### Task 10.10: http.request ノード
- **area**: data / **path**: `crates/workflow-engine`
- **仕様**: egress allowlist × シークレット宛先束縛の AND。タイムアウト・サイズ上限・リトライポリシ。
- **受け入れ条件**:
  - [ ] allowlist 外・束縛外への送信が遮断される
  - [ ] 応答が step 出力として次ノードに渡る

### Task 10.11: skill＆スキルストア
- **area**: app / **path**: `crates/app-platform`（skill 種）, `sdk/`
- **仕様**: skill artifact（指示文＋script 参照＋宣言ツール/スコープ＋資料）。9.13 レジストリ再利用
  （不変 publish・信頼ティア・同意インストール・署名バンドル）。呼び出し面: エージェントマウント／skill ノード
  （`skill:<name>@<version>`・保存時存在検証）。実効=宣言スコープ ∩ 実行主体 ReBAC。
- **受け入れ条件**:
  - [ ] skill を publish→同意インストール→エージェント/ワークフロー両方から呼べる
  - [ ] skill の宣言スコープ外の能力呼び出しが拒否される
  - [ ] 未インストール/存在しない skill を参照する IR が保存時に拒否される

### Task 10.12: dnd ワークフローエディタ
- **area**: frontend / **path**: `web/`
- **仕様**: IR の直接編集（ノード配置・接続・右パネル設定）。設定パネルは codegen 型（10.6）駆動。
  保存時にサーバ側検証（10.1）を通す。
- **受け入れ条件**:
  - [ ] dnd で作成→保存→実行が一気通貫で動く
  - [ ] 検証エラー（語彙違反等）が該当ノード上に表示される

### Task 10.13: AI 編集
- **area**: ai / **path**: `crates/agent-core`, `web/`
- **仕様**: チャット/エディタ内 AI に「IR を生成・変更するツール」を与える（generative UI と同じ検証済みスペック方式）。
  AI 出力は必ず 10.1 検証を通り、**dnd でそのまま人間が続きを編集できる**。
- **受け入れ条件**:
  - [ ] 自然言語からワークフローが生成され、dnd に表示・編集できる
  - [ ] AI が実在しないツール/スコープを参照した場合に保存前に拒否される

### Task 10.14: 実行履歴 UI＋Observability
- **area**: obs / **path**: `web/`, `crates/workflow-engine`
- **仕様**: run/step テーブルを正とする実行履歴（入出力プレビュー・リトライ経過・失敗ステップ・再実行操作）。
  OTel span（run→step）。AI ノードは Langfuse trace_id 突合（3.8 枠）。
- **受け入れ条件**:
  - [ ] 失敗 run の原因ステップと入出力（シークレットはレダクト）を UI で辿れる
  - [ ] 監査↔OTel↔Langfuse が run_id/trace_id で相関する

### Task 10.15: first-party skill 初期セット
- **area**: app / **path**: `sdk/`, レジストリ
- **仕様**: Slack/メール通知等、http.request ラップの公式 skill を少数提供（first-party 署名ティア）。
  ネイティブコネクタは作らない方針の実証を兼ねる。
- **受け入れ条件**:
  - [ ] Slack 通知 skill がシークレット宛先束縛付きで動く
  - [ ] first-party 署名により管理者の個別同意なしで利用可能（信頼ティア動作確認）
