# Phase 3 — チャット＋RAG（★最初のデモ可能な製品）

> 目的: 第一の縦スライスを完成させる。permission-aware RAG を道具に持つLLMチャットを、ストリーミング・引用表示・
> スレッド共有・LLM可視化まで備えて提供する。ここで**初めて顧客にデモできる製品**になる。
> 完了の定義(DoD): ユーザーがチャットで質問すると、LLMが必要に応じて自動で文書検索ツールを使い、
> 権限を守った引用付き回答をストリーミングで返し、その全過程が Langfuse と監査ログに記録される。
>
> ⚠️ **着手前に [設計上の落とし穴](../design-caveats.md) の PIT-31（ジョブ駆動チャット生成の整合性: outbox/lease-fencing/event-log/replay-subscribe/AuthContext 伝播）・
> PIT-10（Phase 2 を Tier-1=file 粒度で先に通す）を確認すること。PIT-9 は取り下げ済み（llm-gateway は LiteLLM Proxy 採用・内部正規形は OpenAI 互換に確定）。**
>
> 📝 **方針（2026-06-29 確定）**: (1) **LLM は LiteLLM Proxy 経由**（llm-gateway は薄いクライアント）。(2) **生成は接続非依存のジョブ**（ページ離脱でも継続）。
> (3) **agent-core はエージェントモード明示 ON のときのみ作動**。通常チャットは古典 RAG 注入＋llm-gateway 直叩き。

## タスク一覧

| ID | タイトル | area | 依存 |
|----|---------|------|------|
| 3.1 | チャットドメインモデル（thread/message/content blocks＋agent_mode＋generation_run/event） | chat | 0.5 |
| 3.2 | `llm-gateway`（LiteLLM Proxy クライアント, OpenAI互換正規形） | agent | 0.3 |
| 3.3 | `agent-core` ループ（制約版）＋`Tool`トレイト（エージェントモード時のみ） | agent | 3.2 |
| 3.4 | `doc_search` ツール（エージェントモード）＋通常チャットの古典RAG注入 | agent | 3.3, 2.10 |
| 3.5 | チャットAPI＋SSEストリーミング（トークン/ツール/引用イベント） | chat | 3.1, 3.3, 3.11 |
| 3.6 | 引用ソース表示＋content blocks レンダリング | frontend | 3.5 |
| 3.7 | スレッド共有（ReBAC） | chat | 3.1, 1.6 |
| 3.8 | Langfuse 計装＋監査ログ突合（trace_id相関） | obs | 3.5, 2.7 |
| 3.9 | ツール自動選択ポリシ（全提示＋権限/破壊系の明示許可・エージェントモード内） | agent | 3.3 |
| 3.10 | チャットUI（会話・ストリーミング・ツール可視化・エージェントモードトグル） | frontend | 3.5, 3.6 |
| 3.11 | チャット生成ジョブ＋ワーカー＋Pub/Sub（接続非依存生成・整合性） | chat | 3.1, 3.3, 1.8 |

---

## 詳細

### Task 3.1: チャットドメインモデル
- **area**: chat / **path**: `crates/chat`, migrations
- **依存**: 0.5
- **仕様**:
  - `thread(id, org, owner, title, agent_mode, created_at)` / `message(id, thread_id, role, parent_id, content JSONB, agent_mode, created_at)`。
    - **`agent_mode`（既定 OFF）**: OFF=通常チャット（古典 RAG 注入＋llm-gateway 直叩き）／ON=エージェントモード（agent-core ツールループ）。thread 既定＋メッセージ単位で上書き可。
  - **content = 構造化ブロック配列**: `text` / `tool_call` / `tool_result` / `citation` / `generative_ui` / `file_ref`。
  - 添付は**ストレージ参照のみ**（実体二重持ち無し）。`parent_id` でブランチ可能構造（UIは線形）。
  - **生成ジョブ用テーブル（Task 3.11 で使用）**:
    - `generation_run(run_id, message_id, status, lease_until, worker_id, fencing_token, cancel_requested, created_at)`
      （status: pending/running/done/failed/cancelled）。冪等キー＝`run_id`（1ターン1 run）。
    - `generation_event(run_id, seq, type, payload JSONB, created_at)` — **append-only・run 毎単調 seq**。部分出力の真実のソース、`message.content` はその projection。
- **受け入れ条件**:
  - [ ] 1メッセージに複数種ブロックを格納/取得できる
  - [ ] 添付がストレージnodeを参照する
  - [ ] ブランチ可能なスキーマだが線形に取得できる
  - [ ] `agent_mode` を thread 既定＋メッセージ単位で保持・取得できる
  - [ ] `generation_run`/`generation_event`（seq 単調・append-only）が定義され、run と message が対応づく

### Task 3.2: `llm-gateway`（LiteLLM Proxy クライアント）
- **area**: agent / **path**: `crates/llm-gateway`, `deploy/`
- **依存**: 0.3
- **仕様**:
  - **LiteLLM Proxy をサイドカー**として配置（compose/k8s）し、`crates/llm-gateway` は **OpenAI 互換 HTTP の薄いクライアント**として実装。`LlmProvider` トレイトはゲートウェイ抽象として残す。
  - **内部正規形 = OpenAI 互換に確定**（[PIT-9](../design-caveats.md) は取り下げ）。
  - **プロバイダ差吸収・フォールバック・リトライ・タイムアウト・ルーティング**は LiteLLM Proxy 設定へ委譲（①ローカルvLLM ②Anthropic ③Gemini（必要なら④Azure））。ストリーミング（SSE/トークン）対応。
  - **shiki 固有責務は gateway 層で担保**（litellm に委ねない）: AuthContext 権限注入・**トークン会計**・コスト計上・Langfuse 相関・監査。
  - セマンティックキャッシュ・高度ルーティング・仮想キーは**後追い**（litellm 機能を順次活用）。
  - ⚠️ 着手時に litellm の Claude 機能透過（thinking / prompt caching / citations）を検証し、不足分の補い方を決める（[PIT-9](../design-caveats.md) 残存リスク）。
- **受け入れ条件**:
  - [ ] LiteLLM Proxy 経由で vLLMと外部API少なくとも1つで生成・ストリーミングできる
  - [ ] プロバイダ差し替えが litellm 設定で可能
  - [ ] トークン数/コストが gateway 層で計上される
  - [ ] deploy に LiteLLM Proxy サービスが追加され、APIキーは proxy 側に環境注入（shiki アプリに置かない）

### Task 3.3: `agent-core` ループ（制約版）＋`Tool`トレイト
- **area**: agent / **path**: `crates/agent-core`
- **依存**: 3.2
- **仕様**:
  - **エージェントモード（`agent_mode` ON）時のみ作動**。通常チャット（OFF）は agent-core を経由せず chat ドメインが llm-gateway を直叩きする。
  - LLM↔ツールのループ（計画→ツール呼出→観測→継続→終了）。**ツールセット非依存**、`Tool` トレイトで差す。
  - Phase 3 は**制約版**: 短ホライズン、チャット会話に介在、ツールは doc_search 等の安全なもの。
  - ツール呼出/結果を content blocks と SSE イベントに変換。エラー回復・最大ステップ制御。
  - **製品の核のため境界・方針の設計に深く関与する。**
- **受け入れ条件**:
  - [ ] モデルがツールを呼び、結果を受けて回答を続けられる
  - [ ] 最大ステップ/タイムアウトで安全に停止する
  - [ ] 同じコアがPhase 4/5でフルツール化できる構造

### Task 3.4: `doc_search` ツール ＋ 通常チャットの古典RAG注入
- **area**: agent / **path**: `crates/agent-core`, `crates/chat`, `crates/rag`
- **依存**: 3.3, 2.10
- **仕様**:
  - **エージェントモード**: `doc_search(query, scope?)` ツール。Phase 2 の permission-aware 検索を**呼び出し時のユーザー権限で**実行し、LLM が自律的に呼び出す。
  - **通常チャット（OFF）**: chat ドメインが Phase 2 検索（Task 2.10）を**事前に直接呼び**、結果を文脈として注入する古典 RAG（ツールループ無し）。
  - いずれも戻りは引用チャンク（content blockの citation に変換）。prompt template の知識スコープがあれば反映、
    ただし**最終可読性は個人ReBACで再チェック**（Task 2.7・post-filter は両モードで必須）。
- **受け入れ条件**:
  - [ ] エージェントモードで LLMが doc_search を呼ぶと権限を守った引用が返る
  - [ ] 通常チャットでも古典RAG注入で権限を守った引用が付く
  - [ ] 呼び出しユーザーの権限が検索に反映される（両モード）
  - [ ] 引用が監査に残る

### Task 3.5: チャットAPI＋SSEストリーミング
- **area**: chat / **path**: `crates/api`, `crates/chat`
- **依存**: 3.1, 3.3, 3.11
- **仕様**:
  - `POST /threads/{id}/messages`（ユーザー発話）→ **outbox TX で user/assistant message 保存＋生成ジョブを pgmq 投入**し **202**（同期実行しない・Task 3.11）。
  - `GET /threads/{id}/stream`（SSE）→ **replay-then-subscribe**: `generation_event` を cursor(`Last-Event-ID`) からリプレイ→Redis Pub/Sub をライブ購読し、**seq で重複排除**して構造化イベント
    （token / tool_call / tool_result / citation / generative_ui / done）を配信。
  - モードフラグで生成経路が分岐（OFF=gateway 直＋古典RAG注入 / ON=agent-core）。ツールイベントも保存（監査/リプレイ/Langfuse）。
- **受け入れ条件**:
  - [ ] トークンが逐次表示され、完了でメッセージが確定保存される
  - [ ] ツール呼出イベントがストリームと保存の両方に出る
  - [ ] 接続断・ページ離脱後に再接続しても、リプレイ＋seq 重複排除で**重複せず途中から再開**できる

### Task 3.6: 引用ソース表示＋content blocks レンダリング
- **area**: frontend / **path**: `web/`
- **依存**: 3.5
- **仕様**:
  - content blocks をレンダリング（text/tool結果/引用カード）。引用は元文書/該当チャンクへリンク。
  - generative_ui ブロックの**プレースホルダ**を用意（実体はPhase 6）。
- **受け入れ条件**:
  - [ ] 回答中の引用から元文書に飛べる
  - [ ] ツール結果が会話内に表示される

### Task 3.7: スレッド共有（ReBAC）
- **area**: chat / **path**: `crates/chat`, `crates/authz`
- **依存**: 3.1, 1.6
- **仕様**:
  - `thread` 型と relations（viewer/commenter/editor）を OpenFGA に追加。共有/解除API、共有相手一覧。
  - 共有されたスレッドの閲覧は閲覧者自身の権限でRAG引用が再評価される点に注意（他人の引用をそのまま見せない設計）。
- **受け入れ条件**:
  - [ ] スレッドを個人/ロールに共有・解除できる
  - [ ] 閲覧権限のないユーザーがアクセスできない

### Task 3.8: Langfuse 計装＋監査突合
- **area**: obs / **path**: `crates/llm-gateway`, `crates/agent-core`, `deploy/`
- **依存**: 3.5, 2.7
- **仕様**:
  - llm-gateway / agent-core を Langfuse 計装（プロンプト/補完/トークン/コスト/ツール/レイテンシ/引用chunk）。**計装は生成ワーカー文脈で行う**（生成は Task 3.11 のワーカー上で走るため）。
  - **LiteLLM Proxy のネイティブ Langfuse 連携**を活用しつつ、shiki カスタム計装（権限・引用chunk・テナント/ユーザー別コスト）と**同一 trace_id で統合**する。
  - **OTel trace_id と Langfuse trace を相関**し、**監査ログ（権限・引用）と trace_id で突合**できるようにする。
    compose に Langfuse（self-host）追加。
- **受け入れ条件**:
  - [ ] 1チャット応答が Langfuse に1トレースとして出る
  - [ ] 引用chunkがトレースに紐づく
  - [ ] 監査ログ↔Langfuse↔OTelが同一trace_idで辿れる

### Task 3.9: ツール自動選択ポリシ
- **area**: agent / **path**: `crates/agent-core`
- **依存**: 3.3
- **仕様**:
  - **エージェントモード内の挙動**（通常チャットはツール自律実行しない）。
  - **デフォルトで利用可能ツールを全提示し、モデルが自動選択**。ユーザーのトグルは許可リスト/ヒント。
  - **権限/破壊的/高コスト系ツールは明示許可**（confirm or 事前許可設定）を要求する仕組み。
- **受け入れ条件**:
  - [ ] ユーザーが選ばなくても適切にdoc_searchが使われる
  - [ ] 破壊系ツールは確認なしに実行されない

### Task 3.10: チャットUI
- **area**: frontend / **path**: `web/`
- **依存**: 3.5, 3.6
- **仕様**:
  - スレッド一覧/新規作成、メッセージ送信、ストリーミング表示、ツール実行の可視化、引用カード、
    ツールのトグル、共有ダイアログ。
  - **エージェントモードのトグル**（既定 OFF＝通常チャット）。
  - **ページ再訪時にイベントログ（generation_event）から途中経過/確定メッセージを復元表示**し、生成中/完了/失敗/キャンセルの状態を表示。
- **受け入れ条件**:
  - [ ] チャットで質問→引用付き回答がストリーミングで得られる
  - [ ] スレッドの作成/切替/共有がUIから完結
  - [ ] ツール実行過程が見える（エージェントモード）
  - [ ] エージェントモードを UI で切り替えられる
  - [ ] 生成中にページを離れて戻っても、途中経過/確定結果と生成状態が復元表示される

### Task 3.11: チャット生成ジョブ＋ワーカー＋Pub/Sub（接続非依存生成・整合性）
- **area**: chat / **path**: `crates/chat`, `crates/agent-core`, `crates/api`, `deploy/`
- **依存**: 3.1, 3.3, 1.8
- **仕様**:
  - チャット送信後に**ページを離れても生成が続く**よう、生成を SSE 接続から分離しジョブ駆動で実行する（設計は design.md §4.4.1 / [PIT-31](../design-caveats.md)）。
  - **アーキ**: API が outbox TX で message 保存＋pgmq へ生成ジョブ enqueue → **生成ワーカー（shiki-server `role=worker`・pgmq 消費プール）**が
    `generation_run` を claim → モード分岐（OFF=llm-gateway 直＋古典RAG注入 / ON=agent-core ループ）で LLM ストリーミング →
    `generation_event` を seq で append（真実のソース）＋ Redis `chat:run:{run_id}` へ publish → 完了時に `message.content` 確定＋status=done。
  - **整合性デザインパターン（必須）**:
    1. **Transactional Outbox**（message 保存＋pgmq enqueue を単一 Postgres TX）。
    2. **Idempotent Consumer ＋ Lease/Fencing**（`run_id` 冪等・`lease_until`/`fencing_token` でクラッシュ takeover とゾンビ書込拒否）。
    3. **Append-only Event Log**（`generation_event` 単調 seq が真実のソース、`message.content` は projection）。
    4. **Replay-then-Subscribe ＋ seq 重複排除**（Redis はベストエフォート、取りこぼしは DB replay で補填）。
    5. **Cooperative Cancellation**（ユーザー明示停止のみ・ページ離脱≠キャンセル）。
    6. **Retry / DLQ**（pgmq visibility-timeout・N 回超で DLQ＋failed 可視化）。
    7. **Orphan reaping**（lease 失効 sweeper＋Phase 5 予算ガード連携）。
    8. **AuthContext 伝播**（ワーカーは発話ユーザー権限で生成し昇格しない・RAG post-filter ライブ再評価）。
  - **デプロイ**: compose/k8s に `role=worker` の shiki-server を追加（API とは別レプリカでスケール可）。
- **受け入れ条件**:
  - [ ] 送信後にページを離れても生成が継続し、途中結果と完了が永続化される（再訪で確認できる）
  - [ ] 再接続で seq リプレイ→重複しない
  - [ ] ワーカークラッシュ時にリース失効で takeover（または failed 化）し、ゾンビ書込が拒否される
  - [ ] ユーザー明示停止でのみキャンセルされ、status=cancelled で部分確定する
  - [ ] 生成ワーカーが発話ユーザーの AuthContext を保持し、RAG post-filter が呼び出しユーザー権限で再評価される
  - [ ] 失敗ジョブが DLQ に入り再実行できる
