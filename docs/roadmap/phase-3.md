# Phase 3 — チャット＋RAG（★最初のデモ可能な製品）

> 目的: 第一の縦スライスを完成させる。permission-aware RAG を道具に持つLLMチャットを、ストリーミング・引用表示・
> スレッド共有・LLM可視化まで備えて提供する。ここで**初めて顧客にデモできる製品**になる。
> 完了の定義(DoD): ユーザーがチャットで質問すると、LLMが必要に応じて自動で文書検索ツールを使い、
> 権限を守った引用付き回答をストリーミングで返し、その全過程が Langfuse と監査ログに記録される。
>
> ⚠️ **着手前に [設計上の落とし穴](../design-caveats.md) の PIT-9（llm-gateway の正規形を OpenAI 互換でなく
> 中立 content-block にし Claude を一級市民にする）・PIT-10（Phase 2 を Tier-1=file 粒度で先に通す）を確認すること。**

## タスク一覧

| ID | タイトル | area | 依存 |
|----|---------|------|------|
| 3.1 | チャットドメインモデル（thread/message/content blocks） | chat | 0.5 |
| 3.2 | `llm-gateway`（in-process, OpenAI互換正規形＋アダプタ） | agent | 0.3 |
| 3.3 | `agent-core` ループ（制約版）＋`Tool`トレイト | agent | 3.2 |
| 3.4 | `doc_search` ツール（Phase 2 検索の接続） | agent | 3.3, 2.10 |
| 3.5 | チャットAPI＋SSEストリーミング（トークン/ツール/引用イベント） | chat | 3.1, 3.3 |
| 3.6 | 引用ソース表示＋content blocks レンダリング | frontend | 3.5 |
| 3.7 | スレッド共有（ReBAC） | chat | 3.1, 1.6 |
| 3.8 | Langfuse 計装＋監査ログ突合（trace_id相関） | obs | 3.5, 2.7 |
| 3.9 | ツール自動選択ポリシ（全提示＋権限/破壊系の明示許可） | agent | 3.3 |
| 3.10 | チャットUI（会話・ストリーミング・ツール可視化） | frontend | 3.5, 3.6 |

---

## 詳細

### Task 3.1: チャットドメインモデル
- **area**: chat / **path**: `crates/chat`, migrations
- **依存**: 0.5
- **仕様**:
  - `thread(id, org, owner, title, created_at)` / `message(id, thread_id, role, parent_id, content JSONB, created_at)`。
  - **content = 構造化ブロック配列**: `text` / `tool_call` / `tool_result` / `citation` / `generative_ui` / `file_ref`。
  - 添付は**ストレージ参照のみ**（実体二重持ち無し）。`parent_id` でブランチ可能構造（UIは線形）。
- **受け入れ条件**:
  - [ ] 1メッセージに複数種ブロックを格納/取得できる
  - [ ] 添付がストレージnodeを参照する
  - [ ] ブランチ可能なスキーマだが線形に取得できる

### Task 3.2: `llm-gateway`（in-process）
- **area**: agent / **path**: `crates/llm-gateway`
- **依存**: 0.3
- **仕様**:
  - `LlmProvider` トレイト実装。**内部正規形 = OpenAI互換スキーマ**。薄いアダプタ: ①ローカルvLLM（ほぼ素通し）
    ②Anthropic ③Gemini（必要なら④Azure）。ストリーミング（SSE/トークン）対応。
    - ⚠️ **この正規形は未決**: [PIT-9](../design-caveats.md) で中立 content-block への変更を検討中
      （Claude の tool_use/thinking/citation/prompt-caching が OpenAI 互換だと綺麗に乗らない）。
      着手前に正規形を確定すること。本記述（OpenAI互換）は確定するまでの暫定。
  - 機能は必要分のみ: フォールバック/リトライ/タイムアウト/**トークン会計**/権限・コスト計上フック。
    セマンティックキャッシュ・高度ルーティング・仮想キーは**後追い**。
- **受け入れ条件**:
  - [ ] vLLMと外部API少なくとも1つで生成・ストリーミングできる
  - [ ] プロバイダ差し替えが設定で可能
  - [ ] トークン数/コストが計上される

### Task 3.3: `agent-core` ループ（制約版）＋`Tool`トレイト
- **area**: agent / **path**: `crates/agent-core`
- **依存**: 3.2
- **仕様**:
  - LLM↔ツールのループ（計画→ツール呼出→観測→継続→終了）。**ツールセット非依存**、`Tool` トレイトで差す。
  - Phase 3 は**制約版**: 短ホライズン、チャット会話に介在、ツールは doc_search 等の安全なもの。
  - ツール呼出/結果を content blocks と SSE イベントに変換。エラー回復・最大ステップ制御。
  - **製品の核のため境界・方針の設計に深く関与する。**
- **受け入れ条件**:
  - [ ] モデルがツールを呼び、結果を受けて回答を続けられる
  - [ ] 最大ステップ/タイムアウトで安全に停止する
  - [ ] 同じコアがPhase 4/5でフルツール化できる構造

### Task 3.4: `doc_search` ツール
- **area**: agent / **path**: `crates/agent-core`, `crates/rag`
- **依存**: 3.3, 2.10
- **仕様**:
  - `doc_search(query, scope?)` ツール。Phase 2 の permission-aware 検索を**呼び出し時のユーザー権限で**実行。
  - 戻りは引用チャンク（content blockの citation に変換）。skill（Phase 6・旧 prompt template）の知識スコープがあれば反映、
    ただし**最終可読性は個人ReBACで再チェック**（Task 2.7）。
- **受け入れ条件**:
  - [ ] LLMが doc_search を呼ぶと権限を守った引用が返る
  - [ ] 呼び出しユーザーの権限が検索に反映される
  - [ ] 引用が監査に残る

### Task 3.5: チャットAPI＋SSEストリーミング
- **area**: chat / **path**: `crates/api`, `crates/chat`
- **依存**: 3.1, 3.3
- **仕様**:
  - `POST /threads/{id}/messages`（ユーザー発話）→ agent-core 実行 → **SSEで構造化イベント逐次配信**
    （token / tool_call / tool_result / citation / generative_ui / done）。完了時に最終メッセージを永続化。
  - ツールイベントも保存（監査/リプレイ/Langfuse）。再接続/中断対応。
- **受け入れ条件**:
  - [ ] トークンが逐次表示され、完了でメッセージが確定保存される
  - [ ] ツール呼出イベントがストリームと保存の両方に出る
  - [ ] 接続断からの復帰で重複しない

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
  - llm-gateway / agent-core を Langfuse 計装（プロンプト/補完/トークン/コスト/ツール/レイテンシ/引用chunk）。
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
- **受け入れ条件**:
  - [ ] チャットで質問→引用付き回答がストリーミングで得られる
  - [ ] スレッドの作成/切替/共有がUIから完結
  - [ ] ツール実行過程が見える
