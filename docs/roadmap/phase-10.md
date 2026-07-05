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

| ID | タイトル | area | 依存 |
|----|---------|------|------|
| 10.1 | ワークフロー IR スキーマ＋artifact 化＋語彙照合検証 | data | 9.1, 6.1 |
| 10.2 | run/step 永続化＋ワーカー（claim/リース/チェックポイント） | data | 10.1 |
| 10.3 | トリガ: スケジューラ（cron・リーダーリース）＋イベントマッチング（outbox） | data | 10.2 |
| 10.4 | 実行主体・委譲モデル（workflow プリンシパル・同意フロー・fail-closed 停止） | auth | 10.2, 9.13 |
| 10.5 | 制御ノード（分岐/並列/join/待機）＋ステップリトライ＋concurrency/rate limit | data | 10.2 |
| 10.6 | 能力ノード（storage/data/rag/notify）＋AI ノード2種＋ノード設定パネル契約 | agent | 10.2, 9.8, 5.1 |
| 10.7 | script-runtime（swc＋wasmtime/QuickJS・Shiki.* ブリッジ・非特権プロセス） | sandbox | – |
| 10.8 | script ノード＋script→ワークフロー起動 API | data | 10.7, 10.2 |
| 10.9 | シークレット管理（crates/secrets・KeyProvider・宛先束縛・監査） | auth | – |
| 10.10 | http.request ノード（egress allowlist × シークレット宛先束縛） | data | 10.9, 10.5 |
| 10.11 | skill artifact＋スキルストア（レジストリ再利用）＋skill ノード/エージェントマウント | app | 9.13, 10.7 |
| 10.12 | dnd ワークフローエディタ（IR 直接編集・ノード設定右パネル） | frontend | 10.1 |
| 10.13 | AI 編集（チャット→IR 生成/変更・検証済みスペック・dnd に引き継ぎ） | ai | 10.12, 6.3 |
| 10.14 | 実行履歴 UI＋Observability（run/step・OTel span・Langfuse 突合） | obs | 10.2, 3.8 |
| 10.15 | first-party skill 初期セット（Slack 通知等・http.request ラップ） | app | 10.10, 10.11 |

---

## 詳細

### Task 10.1: ワークフロー IR スキーマ＋artifact 化＋語彙照合検証
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
- **area**: data / **path**: `crates/workflow-engine`
- **仕様**: cron 式を Postgres 保持・**リーダーリース付き単一スケジューラループ**が due run を enqueue（多重発火防止）。
  イベントトリガは既存 outbox（storage 書込・record 変更・status 遷移）とトリガテーブルのマッチング。
- **受け入れ条件**:
  - [ ] 複数インスタンス起動時もスケジュールが1回だけ発火する
  - [ ] storage 書込／record status 遷移でワークフローが起動する
  - [ ] 無効化済みワークフローのトリガが発火しない

### Task 10.4: 実行主体・委譲モデル（FR-12 最重要）
- **area**: auth / **path**: `crates/workflow-engine`, `crates/authz`
- **仕様**: `authz::Namespace` に `workflow()` ビルダ追加（`workflow:<tenant>|<id>`）。
  対話トリガ=本人 ReBAC ∩ 宣言スコープ ∩ ノード設定。スケジュール/イベント=専用プリンシパルへ
  **有効化時に有効化者が自分の権限範囲内から明示委譲**（同意 UI・9.13 パターン再利用）。
  委譲タプルは委譲者にリンクし、**委譲者の失権・退職で該当ワークフローを fail-closed 停止→再同意要求**。
  監査: run_id・トリガ種別・実行プリンシパル・委譲者。
- **受け入れ条件**:
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
- **area**: sandbox / **path**: `crates/script-runtime`
- **仕様**: swc で TS→JS、**wasmtime 上の QuickJS**（javy 方式）で実行。専用**非特権プロセス**・RPC・インスタンス使い捨て。
  fuel/メモリ上限/epoch interruption。`Shiki.*` ホスト関数ブリッジ（同期スタイル→ホスト側 async 橋渡し）は
  能力ゲートウェイへ合流し AuthContext 認可・監査を通る。npm import 不可。
- **受け入れ条件**:
  - [ ] `Shiki.data.query(...)` 同期スタイルの script が実行でき、認可・監査が通常経路で効く
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
