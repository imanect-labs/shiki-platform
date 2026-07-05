"""FastAPI アプリ本体。

/healthz はプロセス生存を返し、モデルのロード状態を併記する（初回ダウンロード中でも
コンテナを healthy 扱いにでき、compose の起動順が壊れない）。
"""

from fastapi import FastAPI

from . import embed, parse, rerank
from .model_registry import get_registry
from .settings import get_settings


def create_app() -> FastAPI:
    app = FastAPI(title="shiki ingestion-worker", docs_url=None, redoc_url=None)
    app.include_router(parse.router)
    app.include_router(embed.router)
    app.include_router(rerank.router)

    @app.get("/healthz")
    def healthz() -> dict:
        registry = get_registry()
        settings = get_settings()
        return {
            "status": "ok",
            "models": {
                "embed": {
                    "id": settings.embed_model,
                    "loaded": registry.embedding_loaded,
                },
                "rerank": {
                    "id": settings.rerank_model,
                    "loaded": registry.reranker_loaded,
                },
            },
        }

    return app


app = create_app()
