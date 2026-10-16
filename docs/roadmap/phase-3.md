# Phase 3 — チャット＋RAG（★最初のデモ可能な製品）

> 目的: 第一の縦スライスを完成させる。permission-aware RAG を道具に持つLLMチャットを、ストリーミング・引用表示・
> スレッド共有・LLM可視化まで備えて提供する。ここで**初めて顧客にデモできる製品**になる。
> 完了の定義(DoD): ユーザーがチャットで質問すると、LLMが必要に応じて自動で文書検索ツールを使い、
> 権限を守った引用付き回答をストリーミングで返し、その全過程が Langfuse と監査ログに記録される。
>
> ⚠️ **着手前に [設計上の落とし穴](../design-caveats.md) の PIT-9（llm-gateway の正規形を OpenAI 互換でなく
> 中立 content-block にし Claude を一級市民にする）・PIT-10（Phase 2 を Tier-1=file 粒度で先に通す）を確認すること。**
> （PIT-9 は中立 content-block で確定・実装済み〔`crates/llm-gateway/src/lib.rs`〕。PIT-10 は Phase 2 で Tier-2 まで一括実装して解消済み。）
>
> **実装突合（2026-10-17・main 329aba0）**: 3.1〜3.11 の中心経路（content blocks・llm-gateway・agent-core・doc_search・
> 接続非依存生成＋SSE・引用表示・スレッド共有・承認ゲート・チャット UI）は本番経路に配線済みで、結合テストは CI の
> Coverage ジョブで実走している。DoD のうち「全過程が Langfuse に記録される」は未達。
> 残り: OTel 無効時に trace_id が作られず Langfuse 無送信・監査 trace_id が NULL #646／Langfuse・監査にツール呼び出し・結果・引用が残らない #647／
> スレッド共有の拒否側テスト欠落 #648／共有されたスレッドの一覧が無い #649／会話 UI が閲覧者の relation を見ない #650／
> Anthropic アダプタが thinking ブロックを送り返さない #651／別プロバイダへのフォールバックと Gemini アダプタ #652／
> run 全体の時間上限が未配線 #653／doc_search → 引用カードの E2E 欠落 #654／ツールのトグル #102／
> jobq の DLQ 再実行の運用入口 #642／承認待ち中のクラッシュで run が再開されない #364

## タスク一覧

| ID | タイトル | area | 依存 |
|----|---------|------|------|
| 3.1 | チャットドメインモデル（thread/message/content blocks） | chat | 0.5 |
| 3.2 | `llm-gateway`（in-process, 中立 content-block 正規形＋アダプタ） | agent | 0.3 |
| 3.3 | `agent-core` ループ（制約版）＋`Tool`トレイト | agent | 3.2 |
| 3.4 | `doc_search` ツール（Phase 2 検索の接続） | agent | 3.3, 2.10 |
| 3.5 | チャットAPI＋SSEストリーミング（トークン/ツール/引用イベント） | chat | 3.1, 3.3 |
| 3.6 | 引用ソース表示＋content blocks レンダリング | frontend | 3.5 |
| 3.7 | スレッド共有（ReBAC） | chat | 3.1, 1.6 |
| 3.8 | Langfuse 計装＋監査ログ突合（trace_id相関） | obs | 3.5, 2.7 |
| 3.9 | ツール自動選択ポリシ（全提示＋権限/破壊系の明示許可） | agent | 3.3 |
| 3.10 | チャットUI（会話・ストリーミング・ツール可視化） | frontend | 3.5, 3.6 |
| 3.11 | チャット生成ジョブ＋ワーカー＋Pub/Sub（接続非依存生成・#82） | chat | 3.1, 3.3, 1.8 |

---

## 詳細

### Task 3.1: チャットドメインモデル
- **area**: chat / **path**: `crates/chat`, migrations（`migrations/0012_chat.sql` ほか）
- **依存**: 0.5
- **仕様**:
  - `thread(id, org, tenant_id, owner, title, agent_mode, deleted_at, created_at, updated_at)` /
    `message(id, thread_id, org, tenant_id, role, parent_id, content JSONB, agent_mode, created_at)`。全行 `tenant_id` 必須。
  - **content = 構造化ブロック配列**: `text` / `tool_call` / `tool_result` / `citation` / `generative_ui` / `file_ref`。
    （後続フェーズで `thinking`・`workflow_ref`・`note_ref`・`document_ref`・各種下書き・`selection_context` が追加されている。
    正本は `crates/chat/src/model.rs` の `ContentBlock`）
  - 添付は**ストレージ参照のみ**（実体二重持ち無し・`file_ref { node_id, name }`）。`parent_id` でブランチ可能構造（UIは線形）。
- **受け入れ条件**:
  - [x] 1メッセージに複数種ブロックを格納/取得できる
  - [x] 添付がストレージnodeを参照する
  - [x] ブランチ可能なスキーマだが線形に取得できる

### Task 3.2: `llm-gateway`（in-process）
- **area**: agent / **path**: `crates/llm-gateway`
- **依存**: 0.3
- **仕様**:
  - `LlmProvider` トレイト実装。**内部正規形 = プロバイダ中立の content-block**（text / thinking / tool_use / tool_result を
    一級市民に持つ・`crates/llm-gateway/src/model.rs`）。アダプタ: ①OpenAI 互換 Chat Completions（vLLM / OpenAI / LiteLLM Proxy 等）
    ②Anthropic ③OpenAI Responses（#535）。Gemini（必要なら Azure）は未実装 → #652。ストリーミング（SSE/トークン）対応。
    （旧: 内部正規形＝OpenAI 互換スキーマ・PIT-9 で検討中の暫定 → 置換: 中立 content-block で確定・実装済み・根拠 PIT-9／`crates/llm-gateway/src/lib.rs`）
  - 機能は必要分のみ: フォールバック/リトライ/タイムアウト/**トークン会計**/権限・コスト計上フック。
    セマンティックキャッシュ・高度ルーティング・仮想キーは**後追い**。
    リトライは接続確立前の Unavailable を同じプロバイダで 1 回だけ再試行する。別プロバイダへのフォールバックは未実装 → #652。
    タイムアウトは最初の応答・無通信・全体の 3 種（#554）。
- **受け入れ条件**:
  - [x] vLLMと外部API少なくとも1つで生成・ストリーミングできる（CI はモックサーバで検証。実 API の live テストは環境変数未設定で skip。
    Anthropic で思考＋ツールを併用した 2 ターン目に thinking ブロックを送り返していない → #651）
  - [x] プロバイダ差し替えが設定で可能
  - [x] トークン数/コストが計上される

### Task 3.3: `agent-core` ループ（制約版）＋`Tool`トレイト
- **area**: agent / **path**: `crates/agent-core`
- **依存**: 3.2
- **仕様**:
  - LLM↔ツールのループ（計画→ツール呼出→観測→継続→終了）。**ツールセット非依存**、`Tool` トレイトで差す。
  - Phase 3 は**制約版**: 短ホライズン、チャット会話に介在、ツールは doc_search 等の安全なもの。
    （Phase 5 で同じ `run_agent` に自律プロファイル〔フルツール・予算・計画・承認〕が載っている）
  - ツール呼出/結果を content blocks と SSE イベントに変換。エラー回復・最大ステップ制御。
  - **製品の核のため境界・方針の設計に深く関与する。**
- **受け入れ条件**:
  - [x] モデルがツールを呼び、結果を受けて回答を続けられる
  - [ ] 最大ステップ/タイムアウトで安全に停止する（最大ステップ・LLM 呼び出しのタイムアウト〔#554〕は実装・テスト済み。
    run 全体の時間上限〔`Budget.deadline`〕は本番で配線されていない → #653）
  - [x] 同じコアがPhase 4/5でフルツール化できる構造

### Task 3.4: `doc_search` ツール
- **area**: agent / **path**: `crates/agent-core`, `crates/rag`
- **依存**: 3.3, 2.10
- **仕様**:
  - `doc_search(query, scope?)` ツール。Phase 2 の permission-aware 検索を**呼び出し時のユーザー権限で**実行。
    （#423 でモデルが `top_k` / `mode`（hybrid・keyword）/ `file_ids`・`folder_ids` を指定できるようにした。絞り込みは知識スコープとの交差で、範囲を広げない）
  - 戻りは引用チャンク（content blockの citation に変換）。skill（Phase 6・旧 prompt template）の知識スコープがあれば反映、
    ただし**最終可読性は個人ReBACで再チェック**（Task 2.7）。
  - 引用は監査 `rag.search`（`cited_chunk_ids`・`cited_file_ids`・`file_decisions`）に残る。
- **受け入れ条件**:
  - [x] LLMが doc_search を呼ぶと権限を守った引用が返る
  - [x] 呼び出しユーザーの権限が検索に反映される
  - [x] 引用が監査に残る

### Task 3.5: チャットAPI＋SSEストリーミング
- **area**: chat / **path**: `crates/api`, `crates/chat`
- **依存**: 3.1, 3.3
- **仕様**:
  - `POST /threads/{id}/messages`（ユーザー発話）は保存＋生成ジョブ投入だけを行い 202 で `run_id` を返す（Task 3.11）。
    生成はワーカーが agent-core で実行し、**SSEで構造化イベント逐次配信**
    （token / tool_call / tool_result / citation / generative_ui / done）。完了時に最終メッセージを永続化。
  - 購読は `GET /threads/{id}/stream`（`Last-Event-ID`＝seq、クエリ `last_event_id` でも可）。
    **DB（`generation_event`）を真実のソースとして seq 順に replay し、Redis は新イベントの起床通知にだけ使う**
    （取りこぼしは短周期の再 replay で補う・`crates/chat/src/store/stream.rs`）。
    （旧: POST の応答で SSE 配信／design §4.4.1 の `GET /runs/:id/events`・「先に購読→後でリプレイ」 →
    置換: 接続非依存生成〔#82〕で `GET /threads/{id}/stream` の DB replay ループに変更）
  - ツールイベントも保存（監査/リプレイ/Langfuse）。再接続/中断対応（停止は `POST /threads/{id}/runs/{run_id}/cancel`）。
- **受け入れ条件**:
  - [x] トークンが逐次表示され、完了でメッセージが確定保存される
  - [x] ツール呼出イベントがストリームと保存の両方に出る
  - [x] 接続断からの復帰で重複しない

### Task 3.6: 引用ソース表示＋content blocks レンダリング
- **area**: frontend / **path**: `web/`
- **依存**: 3.5
- **仕様**:
  - content blocks をレンダリング（text/tool結果/引用カード）。引用は元文書/該当チャンクへリンク。
  - generative_ui ブロックの**プレースホルダ**を用意（実体はPhase 6）。
    （旧: プレースホルダ → 置換: Phase 6 で `emit_ui` の実体を実装済み。現在は Yui〔```ui〕へ移行中・#604）
- **受け入れ条件**:
  - [x] 回答中の引用から元文書に飛べる（ノート・Office・PDF の該当箇所へ・#508）
  - [x] ツール結果が会話内に表示される（`web/src/components/chat/tool-activity.tsx`・#386）

### Task 3.7: スレッド共有（ReBAC）
- **area**: chat / **path**: `crates/chat`, `crates/authz`
- **依存**: 3.1, 1.6
- **仕様**:
  - `thread` 型と relations（viewer/commenter/editor）を OpenFGA に追加。共有/解除API、共有相手一覧。
    （`POST/DELETE/GET /threads/{id}/shares`・owner のみ。投稿は editor 以上。commenter に対応するコメント機能は無い）
  - 共有されたスレッドの閲覧は閲覧者自身の権限でRAG引用が再評価される点に注意（他人の引用をそのまま見せない設計）。
    ツール結果の本文は実行主体本人にだけ見せる。
- **受け入れ条件**:
  - [x] スレッドを個人/ロールに共有・解除できる（共有された側のスレッド一覧は無い → #649）
  - [ ] 閲覧権限のないユーザーがアクセスできない（実装済み・検証テスト欠落 #648）

### Task 3.8: Langfuse 計装＋監査突合
- **area**: obs / **path**: `crates/llm-gateway`, `crates/agent-core`, `deploy/`
- **依存**: 3.5, 2.7
- **仕様**:
  - llm-gateway / agent-core を Langfuse 計装（プロンプト/補完/トークン/コスト/ツール/レイテンシ/引用chunk）。
    現状は LLM 呼び出しごとの generation のみで、ツール・引用 chunk は送っていない → #647。
  - **OTel trace_id と Langfuse trace を相関**し、**監査ログ（権限・引用）と trace_id で突合**できるようにする。
    compose に Langfuse（self-host）追加（済み）。
    現状は OTel 有効時にしか trace_id が作られず、既定の compose（`OTLP_ENDPOINT` 空）では Langfuse に何も送られない → #646。
- **受け入れ条件**:
  - [ ] 1チャット応答が Langfuse に1トレースとして出る（OTel 有効時のみ → #646）
  - [ ] 引用chunkがトレースに紐づく（引用 chunk は監査 `rag.search` にのみ残り Langfuse には無い → #647）
  - [ ] 監査ログ↔Langfuse↔OTelが同一trace_idで辿れる（OTel 無効時は trace_id が NULL・突合のテストが無い → #646／#647）

### Task 3.9: ツール自動選択ポリシ
- **area**: agent / **path**: `crates/agent-core`
- **依存**: 3.3
- **仕様**:
  - **デフォルトで利用可能ツールを全提示し、モデルが自動選択**。ユーザーのトグルは許可リスト/ヒント（トグルは未実装 → #102）。
    出番の限られるツールは名前だけを提示し（多ければ名前空間ごとの件数・#519）、定義は `tool_search` で
    読み込ませる（#511・design §4.4）。
  - **権限/破壊的/高コスト系ツールは明示許可**（confirm or 事前許可設定）を要求する仕組み。
- **受け入れ条件**:
  - [x] ユーザーが選ばなくても適切にdoc_searchが使われる
  - [x] 破壊系ツールは確認なしに実行されない

### Task 3.10: チャットUI
- **area**: frontend / **path**: `web/`
- **依存**: 3.5, 3.6
- **仕様**:
  - スレッド一覧/新規作成、メッセージ送信、ストリーミング表示、ツール実行の可視化、引用カード、
    ツールのトグル（未実装 → #102）、共有ダイアログ。
  - モデル選択と思考強度（FR-4・#553）はコンポーザの「＋」メニューから選ぶ。
  - 閲覧者の relation に応じた表示制御（viewer に入力欄・共有ボタンを出さない）は未実装 → #650。
- **受け入れ条件**:
  - [ ] チャットで質問→引用付き回答がストリーミングで得られる（実装済み・検証テスト欠落 #654）
  - [x] スレッドの作成/切替/共有がUIから完結（共有された側の一覧 → #649）
  - [x] ツール実行過程が見える

### Task 3.11: チャット生成ジョブ＋ワーカー＋Pub/Sub（接続非依存生成）
- **area**: chat / **path**: `crates/chat`, `crates/agent-core`, `crates/api`, `migrations/0012_chat.sql`
- **依存**: 3.1, 3.3, 1.8
- **出典**: #82（closed）。設計は design §4.4.1。
- **仕様**:
  - チャット送信後に**ページを離れても生成が続く**よう、生成を SSE 接続から分離しジョブ駆動で実行する。
  - **投入**: `POST /threads/{id}/messages` が単一 Tx で user/assistant message 保存＋`generation_run` 行＋jobq enqueue
    （outbox 型）。202 で `run_id` を即返す。
  - **実行**: shiki-server 内のワーカープール（`SHIKI__CHAT__WORKER_CONCURRENCY`）が run を claim
    （queued→running・`lease_until`/`worker_id`/`fencing_token`）。リース失効で別ワーカーが takeover し、旧ワーカーの書込は fencing で拒否する。
    ワーカーは発話ユーザー（`actor`）の AuthContext で生成し昇格しない。
  - **イベント**: `generation_event(run_id, seq)` を単調 seq で追記（unique で exactly-once）＝真実のソース。`message.content` はその projection。
  - **購読**: `GET /threads/{id}/stream` が `Last-Event-ID`(seq) 以降を DB replay＋Redis 起床通知で配信（Task 3.5）。
  - **キャンセル**: ユーザー明示停止（`cancel_requested`）でのみ。ページ離脱≠キャンセル。
  - **再開**: 自律 run はステップ境界のチェックポイントから再開する（#351）。
    承認待ちが jobq の可視性タイムアウトを超えた後のクラッシュでは再開されない → #364。
- **受け入れ条件**:
  - [x] 送信後にページを離れても生成が継続し、途中結果と完了が永続化される（再訪で確認できる）
  - [x] 再接続で seq リプレイ→重複しない
  - [x] ワーカークラッシュ時にリース失効で takeover（または failed 化）し、ゾンビ書込が拒否される
  - [x] ユーザー明示停止でのみキャンセルされ、status=cancelled で部分確定する
  - [x] 生成ワーカーが発話ユーザーの AuthContext を保持し、RAG post-filter が呼び出しユーザー権限で再評価される
  - [ ] 失敗ジョブが DLQ に入り再実行できる（DLQ 行きと run の failed 化は実装済み。再実行の運用入口が無い → #642）
  - [x] 生成ワーカーが shiki-server 内で動く
    （旧: deploy に `role=worker` の shiki-server を追加 → 置換: shiki-server 内の tokio ワーカープール・design §4.4.1）
