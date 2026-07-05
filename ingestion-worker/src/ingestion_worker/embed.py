"""POST /embed — Ruri 埋め込み。

Ruri v3 の非対称プレフィックス（`検索クエリ: ` / `検索文書: `）はここで付与する。
呼び出し側（Rust EmbeddingProvider）は input_type を渡すだけでよく、モデル固有の
プレフィックス知識を持たない（モデル差し替えを worker 内に閉じる）。
"""

from fastapi import APIRouter

from .model_registry import get_registry
from .schemas import EmbedInputType, EmbedRequest, EmbedResponse
from .settings import get_settings

router = APIRouter()

# Ruri v3 系のプレフィックス（モデルカード準拠）。
_QUERY_PREFIX = "検索クエリ: "
_DOCUMENT_PREFIX = "検索文書: "


@router.post("/embed")
def embed(req: EmbedRequest) -> EmbedResponse:
    prefix = _QUERY_PREFIX if req.input_type == EmbedInputType.QUERY else _DOCUMENT_PREFIX
    texts = [prefix + t for t in req.texts]
    vectors = get_registry().embedding().encode(texts)
    dimension = len(vectors[0]) if vectors else 0
    return EmbedResponse(
        vectors=vectors,
        model_version=get_settings().embed_model,
        dimension=dimension,
    )
