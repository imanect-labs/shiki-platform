"""FastAPI アプリ本体。

/healthz はプロセス生存を返し、モデルのロード状態を併記する（初回ダウンロード中でも
コンテナを healthy 扱いにでき、compose の起動順が壊れない）。

起動時にモデルの読み込みと空打ちを**バックグラウンドで**始める（`warmup_models`）。
/healthz は待たせない。読み込み中に来た推論は、読み込みの終わりを待ってから走る。
"""

import threading
from collections.abc import AsyncIterator
from contextlib import asynccontextmanager

from fastapi import FastAPI

from . import edit, embed, parse, rerank
from .model_registry import get_registry
from .settings import get_settings


@asynccontextmanager
async def _lifespan(_app: FastAPI) -> AsyncIterator[None]:
    if get_settings().warmup_models:
        threading.Thread(target=get_registry().warmup, name="model-warmup", daemon=True).start()
    yield


def create_app() -> FastAPI:
    app = FastAPI(title="shiki ingestion-worker", docs_url=None, redoc_url=None, lifespan=_lifespan)
    app.include_router(parse.router)
    app.include_router(edit.router)
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
