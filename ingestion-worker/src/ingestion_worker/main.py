"""FastAPI アプリ本体。

/healthz はプロセス生存を返し、モデルのロード状態を併記する（初回ダウンロード中でも
コンテナを healthy 扱いにでき、compose の起動順が壊れない）。/healthz だけは認証なしで
応え、他は共有トークンを要求する（security.py・#812）。

起動時にモデルの読み込みと空打ちを**バックグラウンドで**始める（`warmup_models`）。
/healthz は待たせない。読み込み中に来た推論は、読み込みの終わりを待ってから走る。

トレース（OTel）は telemetry.py。受信した traceparent を継続し、/healthz は外す。
"""

import threading
from collections.abc import AsyncIterator
from contextlib import asynccontextmanager

from fastapi import Depends, FastAPI
from opentelemetry.trace import TracerProvider

from . import edit, embed, parse, rerank
from .model_registry import get_registry
from .security import log_security_posture, require_worker_token
from .settings import get_settings
from .telemetry import setup_tracing


@asynccontextmanager
async def _lifespan(_app: FastAPI) -> AsyncIterator[None]:
    if get_settings().warmup_models:
        threading.Thread(target=get_registry().warmup, name="model-warmup", daemon=True).start()
    yield


def create_app(tracer_provider: TracerProvider | None = None) -> FastAPI:
    """`tracer_provider` はテスト用。未指定なら OTEL_EXPORTER_OTLP_ENDPOINT から作る。"""
    app = FastAPI(title="shiki ingestion-worker", docs_url=None, redoc_url=None, lifespan=_lifespan)
    log_security_posture()
    # /healthz（compose の healthcheck・dev-up の疎通確認）以外は共有トークンを要求する（#812）。
    authed = [Depends(require_worker_token)]
    for router in (parse.router, edit.router, embed.router, rerank.router):
        app.include_router(router, dependencies=authed)

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

    setup_tracing(app, tracer_provider)
    return app


app = create_app()
