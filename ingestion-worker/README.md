# ingestion-worker

shiki-platform のインジェスト推論サービス（Python / FastAPI）。ステートレスで、
shiki-server の `DocumentParser` / `EmbeddingProvider` / `Reranker` トレイト（HTTP 実装）
から呼ばれる。**全リクエスト DTO に `tenant_id` が必須**（docs/design.md §4.3）。

| エンドポイント | 役割 |
|---|---|
| `POST /parse` | Docling で PDF/docx/pptx/xlsx/md 等を構造化ブロック列へ（表は Markdown 化・スキャン PDF は Tesseract 日本語 OCR） |
| `POST /embed` | Ruri v3 埋め込み（`検索クエリ: `/`検索文書: ` プレフィックスはここで付与） |
| `POST /rerank` | 日本語 cross-encoder の関連度スコア |
| `GET /healthz` | プロセス生存＋モデルロード状態 |

## 開発

```bash
uv sync                 # 依存インストール（torch は CPU ホイール固定）
uv run ruff check .     # lint
uv run pytest           # 高速テスト（フェイクモデル・CI 常時実行）
uv run pytest -m slow   # Docling 実走（重い・opt-in。PDF は WORKER_MODEL_TESTS=1 も必要）
uv run uvicorn ingestion_worker.main:app --reload  # ローカル起動
```

モデルは初回リクエスト時に `HF_HOME`（compose では `hf-cache` volume）へダウンロードされる。
埋め込みモデルを変えるときは Rust 側 `SHIKI__RAG__EMBEDDING_MODEL_VERSION` も揃えること
（不一致はインジェスト時に version 突合ガードで拒否 → インデックスは shadow 再構築で移行。
docs/design.md §4.3 PIT-8）。
