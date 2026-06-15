---
name: architecture-invariants
description: shiki-platform のアーキテクチャ/セキュリティ不変条件の詳細チェックリスト。api・authz・storage・rag・chat・agent-core・sandbox・app-gateway・data 等のコードを書く/レビューする際、または認可・監査・公開API・構造化データに触れる際に使う。
---

# アーキテクチャ不変条件チェックリスト

docs/design.md を正本とする実装時の遵守事項。ここはチェックリストであり、設計の根拠・全体像は design.md を読むこと。

## 単一チョークポイント（design §1, §4.2, §4.5）

- ファイル I/O は StorageService を経由したか。実体（オブジェクトストア）に直接権限を持たせていないか。
- 認可判定は OpenFGA クライアント経由の単一の問いに帰着しているか。個別ハンドラに権限ロジックを散らしていないか。
- LLM 呼び出しは llm-gateway 経由か（フォールバック/トークン会計/Langfuse 計装/権限注入をそこで通す）。
- 「エンドポイント → 必要スコープ」は宣言的マップで一律強制し、ハンドラ個別チェックにしていないか。

## 認可コンテキスト（design §4.1）

- 全データアクセスは AuthContext { principal, org } を受け取っているか。アンビエント権限（暗黙のグローバル権限）に依存していないか。
- 将来の tenant_id 追加の継ぎ目を壊していないか。

## 二段 authz（design §4.3, §4.10）

- RAG 検索は pre-filter（可読タグ）＋ post-filter（OpenFGA 検証）の両方を通すか。片方が壊れても権限が守られるか。
- 構造化データ（crates/data）は テーブル ReBAC ＋ クエリ時述語(ABAC, WHERE 強制付与) を通すか。集計・ビューでも述語が適用されバイパス不可か。
- 公開 API（app-gateway）は 二重ゲート = アプリスコープ ∩ ユーザー ReBAC を満たすか。B2 は token-exchange でユーザー代理を維持しているか（confused-deputy 防御）。

## トレイト差し替え点（design §3.1）

- cloud/onprem の差は ObjectStore / VectorStore / LlmProvider / Sandbox / DocumentParser / EmbeddingProvider のトレイト実装に閉じているか。アプリ本体に分岐を持ち込んでいないか。

## codegen を正とする（design §4.1, §5）

- 型は Rust → OpenAPI(utoipa) → openapi-typescript、SSE イベント型は ts-rs/typeshare で生成し、手書き型を作っていないか。
- 認可語彙（OpenFGA relation／能力スコープ <能力>.<操作>／許可ツール名／宣言的アクション ID）を単一定義から Rust enum ＋ TS 型へ生成しているか。実在しない relation/スコープ/ツール名を閉じた集合で弾けるか（LLM ハルシネーション境界）。

## 監査・可観測性（design §4.9）

- 認可・引用 chunk の監査ログと Langfuse を trace_id で突合できるよう種を蒔いているか。

## 正しさ・セキュリティクリティカル領域

sandbox-orchestrator / fuse / agent-core ループ / RAG 二段authz / app-gateway(OAuth2・token-exchange・二重ゲート) / data 行レベル述語エンジン は正しさ・セキュリティクリティカル。境界を明確にし、人がレビューする。OpenFGA relation schema の決定は人が握る。
