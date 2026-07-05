"""HTTP DTO。

不変条件（docs/design.md §4.3）: **全リクエストに tenant_id が必須**。worker は
ステートレスだが、経路全体で tenant を第一級フィールドとして通し、ログ・監査・
将来の per-tenant 制限の根拠にする（free-form payload に落とさない）。
"""

from enum import StrEnum

from pydantic import BaseModel, Field


class BlockType(StrEnum):
    """パース結果の構造化ブロック種別。"""

    HEADING = "heading"
    PARAGRAPH = "paragraph"
    TABLE = "table"
    CAPTION = "caption"


class ParsedBlock(BaseModel):
    """文書の読み順に並んだ構造化ブロック。表は Markdown 化したテキストを持つ。"""

    type: BlockType
    # heading のみ: 見出しレベル（1 が最上位）。
    level: int | None = None
    text: str
    page: int | None = None


class ParseRequest(BaseModel):
    tenant_id: str = Field(min_length=1)
    # StorageService が発行した内部向け・短 TTL の presigned GET URL。
    source_url: str = Field(min_length=1)
    content_type: str = Field(min_length=1)
    file_name: str = Field(min_length=1)


class ParseResponse(BaseModel):
    blocks: list[ParsedBlock]
    # OCR を実行したか（スキャン PDF の可視化・デバッグ用）。
    used_ocr: bool = False


class EmbedInputType(StrEnum):
    """Ruri v3 の非対称プレフィックス（クエリ/文書）を worker 側で付与するための区別。"""

    QUERY = "query"
    DOCUMENT = "document"


class EmbedRequest(BaseModel):
    tenant_id: str = Field(min_length=1)
    input_type: EmbedInputType
    texts: list[str] = Field(min_length=1, max_length=256)


class EmbedResponse(BaseModel):
    vectors: list[list[float]]
    # 実際にロードしているモデル ID。Rust 側が設定値と突合する（PIT-8 ガード）。
    model_version: str
    dimension: int


class RerankPassage(BaseModel):
    id: str
    text: str


class RerankRequest(BaseModel):
    tenant_id: str = Field(min_length=1)
    query: str = Field(min_length=1)
    passages: list[RerankPassage] = Field(min_length=1, max_length=256)


class RerankScore(BaseModel):
    id: str
    score: float


class RerankResponse(BaseModel):
    scores: list[RerankScore]
    model_version: str


class ParseErrorResponse(BaseModel):
    """パース失敗の構造化エラー（握りつぶさず 422 で返し、Rust 側で記録する）。"""

    error: str
    detail: str
