# Phase 2 — RAG（インジェスト＋検索）

> 目的: 「最もベストな permission-aware RAG」を成立させる。ストレージ(Phase 1)の文書を高品質に構造化して索引し、
> **権限を厳密に守った（二段authz）引用付き検索**をAPIとして提供する。
> 完了の定義(DoD): ファイルをストレージに置くと自動で索引され、ユーザーのクエリに対し「そのユーザーが読める文書だけ」
> から引用チャンク付きの検索結果が返り、引用が監査ログに残る。

## タスク一覧

| ID | タイトル | area | 依存 |
|----|---------|------|------|
| 2.1 | `DocumentParser` トレイト＋Docling worker（パース/OCR） | rag | 1.8 |
| 2.2 | チャンク化（レイアウト/親子）＋メタデータ/authz_tags | rag | 2.1 |
| 2.3 | `EmbeddingProvider` トレイト＋Ruri 埋め込み | rag | 2.2 |
| 2.4 | `VectorStore`（Qdrant）索引＋pre-filterタグ | rag | 2.3 |
| 2.5 | 全文検索（Tantivy＋Lindera）索引 | rag | 2.2 |
| 2.6 | ハイブリッド検索＋RRF融合＋reranker | rag | 2.4, 2.5 |
| 2.7 | permission-aware 二段authzフィルタ＋引用監査 | rag | 2.6, 1.9 |
| 2.8 | インジェスト・パイプライン配線（イベント→キュー→worker） | rag | 1.8, 2.1 |
| 2.9 | 増分再索引＋削除/移動の索引整合 | rag | 2.8 |
| 2.10 | 検索API＋デバッグUI（引用ハイライト） | rag | 2.7 |

---

## 詳細

### Task 2.1: `DocumentParser` トレイト＋Docling worker
- **area**: rag / **path**: `ingestion-worker/`, `crates/rag`
- **依存**: 1.8
- **仕様**:
  - `ingestion-worker`（Python）に Docling を組込み、PDF/docx/pptx/xlsx を**レイアウト・表・読み順**込みで構造化。
    スキャンPDFは**日本語OCR**（PaddleOCR/Tesseract+jpn, Docling内包）を有効化。
  - shiki-server 側は `DocumentParser` トレイトで抽象化（gRPC/HTTPでworker呼出）。差し替え可能に。
  - 出力は構造化中間表現（見出し階層・段落・表をMarkdown化・図キャプション）。
- **受け入れ条件**:
  - [ ] 表を含むPDF/Excelが表構造を保って抽出される
  - [ ] スキャンPDF（日本語）からテキストが得られる
  - [ ] パース失敗が握りつぶされずエラーとして記録される

### Task 2.2: チャンク化＋メタデータ
- **area**: rag / **path**: `crates/rag` or worker
- **依存**: 2.1
- **仕様**:
  - **レイアウト/セマンティック・チャンク化**（見出し・段落・表境界を尊重）。表は表単位で保持。
  - **親子チャンク（small-to-big）**: 小チャンクで検索、親（節）を文脈としてLLMに渡せる構造。
  - 各チャンクに `doc_id, page, 見出しパス, authz_tags, embedding_model_version` を付与。
    **authz_tags は元ファイルの可読性に対応**（pre-filterの鍵、Task 2.7と整合）。
- **受け入れ条件**:
  - [ ] 表が分割で壊れない
  - [ ] 小チャンク↔親チャンクの対応が引ける
  - [ ] 全チャンクに authz_tags と model version が付く

### Task 2.3: `EmbeddingProvider`＋Ruri 埋め込み
- **area**: rag / **path**: `crates/rag`, 推論サービス
- **依存**: 2.2
- **仕様**:
  - `EmbeddingProvider` トレイト。デフォルト **Ruri（日本語特化, self-host）**。BGE-m3/e5へ差し替え可能。
  - 推論は llm-gateway とは別の埋め込み推論エンドポイント（ローカルGPU or 外部）。バッチ埋め込み対応。
  - `embedding_model_version` を固定し、**変更＝該当インデックス全再構築**を強制するガード。
- **受け入れ条件**:
  - [ ] チャンクがバッチで埋め込まれる
  - [ ] モデル差し替えが設定で可能
  - [ ] version 不一致のベクタ混在を検出・拒否

### Task 2.4: `VectorStore`（Qdrant）索引＋pre-filterタグ
- **area**: rag / **path**: `crates/rag`
- **依存**: 2.3
- **仕様**:
  - `VectorStore` トレイト＋Qdrant実装。ペイロードに `authz_tags`/`doc_id`/メタを格納し**フィルタ付きANN**。
    小規模向けに pgvector 実装も差し替え可能に（Phase 8 でも可）。
  - upsert/delete/search（フィルタ付き）を提供。
- **受け入れ条件**:
  - [ ] authz_tags フィルタ付き検索が正しく絞り込む
  - [ ] doc 削除でベクタも消える
  - [ ] トレイトで pgvector へ差し替えできる構造

### Task 2.5: 全文検索（Tantivy＋Lindera）
- **area**: rag / **path**: `crates/rag`
- **依存**: 2.2
- **仕様**:
  - Tantivy インデックスに同チャンクを格納、**Lindera で日本語形態素**トークナイズ。BM25検索。
  - **authz_tags を Tantivy 側にも持たせ pre-filter を適用**（dense と同じ権限境界）。
  - dense と同一の doc/chunk ID 体系で突合可能に。
- **受け入れ条件**:
  - [ ] 日本語キーワードが形態素で正しくヒットする
  - [ ] authz_tags フィルタが全文側にも効く
  - [ ] dense とID整合が取れRRFに渡せる

### Task 2.6: ハイブリッド検索＋RRF＋reranker
- **area**: rag / **path**: `crates/rag`
- **依存**: 2.4, 2.5
- **仕様**:
  - dense(Qdrant) と keyword(Tantivy) の結果を **RRF** で融合 → **reranker**（bge/japanese-reranker, 差し替え可）で並べ替え。
  - top-k/しきい値/親子展開のパラメータ化。融合の正しさ（順位・重複排除）を厳密に。
- **受け入れ条件**:
  - [ ] 融合結果が dense/keyword 単独より関連性が高い（評価セットで確認）
  - [ ] 重複チャンクが除去される
  - [ ] reranker を差し替えられる

### Task 2.7: permission-aware 二段authzフィルタ＋引用監査
- **area**: rag / **path**: `crates/rag`, `crates/authz`, `crates/storage`
- **依存**: 2.6, 1.9
- **仕様**:
  - **pre-filter**: 検索時にユーザーの可読 authz_tags で dense/keyword 両系統を絞る。
  - **post-filter 検証**: 取得後に **OpenFGA で最終 check**（authz_tags が陳腐化していても権限変更に追従）。
    片方が壊れても権限を守る二重防御。
  - **引用監査**: 最終的にLLMへ渡す/UIに出す **chunk_id 群＋その時の認可判定を監査ログに記録**（trace_id付き）。
  - 「閲覧不可は検索結果にも回答にも絶対混入しない」を満たすことをテストで保証。
- **受け入れ条件**:
  - [ ] 権限剥奪直後にそのユーザーの検索からchunkが消える（post-filterで）
  - [ ] 混入ゼロのadversarialテスト（共有解除/部署異動シナリオ）が通る
  - [ ] 引用chunkと認可判定が監査ログに残る

### Task 2.8: インジェスト・パイプライン配線
- **area**: rag / **path**: `crates/rag`, `ingestion-worker/`
- **依存**: 1.8, 2.1
- **仕様**:
  - StorageService の書込イベント（1.8）→ ジョブキュー（pgmq）→ worker（parse→chunk→embed→index）。
  - リトライ/デッドレター、進捗・失敗の可視化。冪等性（同一版の二重処理を防ぐ）。
- **受け入れ条件**:
  - [ ] アップロードから数秒〜分で検索可能になる
  - [ ] 失敗ジョブがDLQに入り再実行できる
  - [ ] 同一版の重複インジェストが起きない

### Task 2.9: 増分再索引＋削除/移動整合
- **area**: rag / **path**: `crates/rag`
- **依存**: 2.8
- **仕様**:
  - 更新は該当ファイルのチャンクのみ差し替え。削除でベクタ/全文/メタを除去。移動で authz_tags 再評価。
  - エージェント（Phase 4/5）がFUSE経由で書いたファイルも同経路で自動再索引。
- **受け入れ条件**:
  - [ ] 更新で古いチャンクが残らない
  - [ ] 削除で全索引から消える
  - [ ] 共有変更で authz_tags が再評価される

### Task 2.10: 検索API＋デバッグUI
- **area**: rag / frontend / **path**: `crates/api`, `web/`
- **依存**: 2.7
- **仕様**:
  - `POST /search`（クエリ→引用付き結果）。デバッグUIで引用元のハイライト・スコア・どの段で絞られたか表示。
  - Phase 3 のチャット doc_search ツールはこのAPIを使う。
- **受け入れ条件**:
  - [ ] 引用付き結果が返り、元文書の該当箇所にジャンプできる
  - [ ] 権限で絞られた件数が分かる（デバッグ表示）
