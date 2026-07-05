"""POST /rerank — 日本語 cross-encoder による並べ替えスコア。"""

from fastapi import APIRouter

from .model_registry import get_registry
from .schemas import RerankRequest, RerankResponse, RerankScore
from .settings import get_settings

router = APIRouter()


@router.post("/rerank")
def rerank(req: RerankRequest) -> RerankResponse:
    pairs = [(req.query, p.text) for p in req.passages]
    raw_scores = get_registry().reranker().predict(pairs)
    scores = [
        RerankScore(id=p.id, score=s) for p, s in zip(req.passages, raw_scores, strict=True)
    ]
    return RerankResponse(scores=scores, model_version=get_settings().rerank_model)
