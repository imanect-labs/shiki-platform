"""POST /rerank — 日本語 cross-encoder による並べ替えスコア。"""

import logging

from fastapi import APIRouter

from .model_registry import get_registry
from .schemas import RerankRequest, RerankResponse, RerankScore
from .settings import get_settings
from .telemetry import inference_span

router = APIRouter()


logger = logging.getLogger(__name__)


@router.post("/rerank")
def rerank(req: RerankRequest) -> RerankResponse:
    logger.info("rerank tenant=%s passages=%d", req.tenant_id, len(req.passages))
    pairs = [(req.query, p.text) for p in req.passages]
    model = get_settings().rerank_model
    attributes = {"worker.model": model, "worker.passages": len(pairs)}
    with inference_span("worker.rerank.predict", attributes):
        raw_scores = get_registry().reranker().predict(pairs)
    scores = [
        RerankScore(id=p.id, score=s) for p, s in zip(req.passages, raw_scores, strict=True)
    ]
    return RerankResponse(scores=scores, model_version=model)
