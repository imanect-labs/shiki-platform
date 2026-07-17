"""Office ファイルのステートレス編集（Task 11.8・/edit）。

設計（docs/design.md §4.8「AI の読み書き」②）:
- **ステートレス bytes 入出力**: リクエストで base64 の文書バイトを受け取り、編集後の
  バイトを base64 で返す。worker はストレージへ一切アクセスしない（認可・保存・
  バージョニングは Rust 側 StorageService のチョークポイントが担う）。
- ops は種別ごとの最小クローズド集合（docx/xlsx/pptx）。未知 op は pydantic で 422。
- 対象不一致（見出しが無い・シートが無い等）は**op 単位の warning** に落として続行し、
  適用数を EditReport で返す（LLM が結果を見て自律的にリトライできる形）。
"""

import base64
import binascii
import logging
from typing import Annotated, Literal

from fastapi import APIRouter, HTTPException
from pydantic import BaseModel, Field, TypeAdapter

from .settings import get_settings

logger = logging.getLogger(__name__)

router = APIRouter()

_DOCX_TYPE = "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
_XLSX_TYPE = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
_PPTX_TYPE = "application/vnd.openxmlformats-officedocument.presentationml.presentation"

# ---------------------------------------------------------------------------
# ops（種別ごとのクローズド集合。Rust 側ツール説明と対にする）
# ---------------------------------------------------------------------------

CellValue = str | int | float | bool | None


class ReplaceTextOp(BaseModel):
    """docx/pptx: 文書中の文字列を置換する（一致数を applied に返す）。"""

    op: Literal["replace_text"]
    find: str = Field(min_length=1, max_length=1000)
    replace: str = Field(max_length=10_000)


class AppendMarkdownOp(BaseModel):
    """docx: 文書末尾に Markdown（見出し/箇条書き/段落の最小集合）を追記する。"""

    op: Literal["append_markdown"]
    markdown: str = Field(min_length=1, max_length=100_000)


class InsertAfterHeadingOp(BaseModel):
    """docx: 指定見出しの直後に Markdown ブロックを挿入する。"""

    op: Literal["insert_after_heading"]
    heading: str = Field(min_length=1, max_length=500)
    markdown: str = Field(min_length=1, max_length=100_000)


class SetCellsOp(BaseModel):
    """xlsx: セル参照（A1 形式）→値の一括設定。"""

    op: Literal["set_cells"]
    sheet: str = Field(min_length=1, max_length=100)
    cells: dict[str, CellValue] = Field(min_length=1, max_length=1000)


class InsertRowsOp(BaseModel):
    """xlsx: 指定行位置（1 始まり）に行を挿入して値を埋める。"""

    op: Literal["insert_rows"]
    sheet: str = Field(min_length=1, max_length=100)
    at: int = Field(ge=1, le=1_048_576)
    rows: list[list[CellValue]] = Field(min_length=1, max_length=1000)


class AddSheetOp(BaseModel):
    """xlsx: シートを追加する（同名は warning でスキップ）。"""

    op: Literal["add_sheet"]
    name: str = Field(min_length=1, max_length=31)


class AddSlideOp(BaseModel):
    """pptx: タイトル＋箇条書きのスライドを末尾に追加する。"""

    op: Literal["add_slide"]
    title: str = Field(min_length=1, max_length=500)
    bullets: list[str] = Field(default_factory=list, max_length=50)


class RemoveSlideOp(BaseModel):
    """pptx: 指定インデックス（0 始まり）のスライドを削除する。"""

    op: Literal["remove_slide"]
    index: int = Field(ge=0, le=10_000)


DocxOp = Annotated[
    ReplaceTextOp | AppendMarkdownOp | InsertAfterHeadingOp, Field(discriminator="op")
]
XlsxOp = Annotated[SetCellsOp | InsertRowsOp | AddSheetOp, Field(discriminator="op")]
PptxOp = Annotated[ReplaceTextOp | AddSlideOp | RemoveSlideOp, Field(discriminator="op")]

_DOCX_OPS: TypeAdapter[list[DocxOp]] = TypeAdapter(list[DocxOp])
_XLSX_OPS: TypeAdapter[list[XlsxOp]] = TypeAdapter(list[XlsxOp])
_PPTX_OPS: TypeAdapter[list[PptxOp]] = TypeAdapter(list[PptxOp])


class EditRequest(BaseModel):
    tenant_id: str = Field(min_length=1)
    content_type: str = Field(min_length=1)
    file_name: str = Field(min_length=1)
    # 編集対象の文書バイト（base64）。上限は settings.max_edit_bytes（デコード後）。
    data_base64: str = Field(min_length=1)
    # 種別ごとの union で二段検証するため、ここでは生 dict で受ける。
    ops: list[dict] = Field(min_length=1, max_length=50)


class OpResult(BaseModel):
    """op 単位の適用結果（applied=置換数/挿入ブロック数など op 固有の件数）。"""

    op: str
    applied: int
    warning: str | None = None


class EditReport(BaseModel):
    """適用サマリ。applied_ops = 1 件以上適用できた op の数。"""

    applied_ops: int
    results: list[OpResult]


class EditResponse(BaseModel):
    data_base64: str
    report: EditReport


def _edit_error(error: str, detail: str) -> HTTPException:
    # /parse と同じ構造化 422（Rust 側 map_worker_error が恒久エラーとして扱う）。
    return HTTPException(status_code=422, detail={"error": error, "detail": detail})


def _validate_ops(content_type: str, raw_ops: list[dict]):
    """種別ごとの op union で二段検証する（未知 op・型不一致は 422）。"""
    from pydantic import ValidationError

    adapter = {
        _DOCX_TYPE: _DOCX_OPS,
        _XLSX_TYPE: _XLSX_OPS,
        _PPTX_TYPE: _PPTX_OPS,
    }.get(content_type)
    if adapter is None:
        raise _edit_error("unsupported_content_type", content_type)
    try:
        return adapter.validate_python(raw_ops)
    except ValidationError as exc:
        raise _edit_error("invalid_ops", str(exc)) from exc


@router.post("/edit")
def edit(req: EditRequest) -> EditResponse:
    """Office 文書へ ops を適用し、編集後のバイトとレポートを返す（ステートレス）。"""
    from . import edit_apply

    content_type = req.content_type.split(";")[0].strip().lower()
    ops = _validate_ops(content_type, req.ops)

    try:
        data = base64.b64decode(req.data_base64, validate=True)
    except (binascii.Error, ValueError) as exc:
        raise _edit_error("invalid_base64", "data_base64 をデコードできません") from exc
    limit = get_settings().max_edit_bytes
    if len(data) > limit:
        raise _edit_error("too_large", f"編集対象が上限を超えています（最大 {limit} バイト）")

    logger.info(
        "edit tenant=%s file=%s type=%s ops=%d",
        req.tenant_id,
        req.file_name,
        content_type,
        len(ops),
    )
    apply_fn = {
        _DOCX_TYPE: edit_apply.apply_docx,
        _XLSX_TYPE: edit_apply.apply_xlsx,
        _PPTX_TYPE: edit_apply.apply_pptx,
    }[content_type]
    try:
        out, tuples = apply_fn(data, ops)
    except HTTPException:
        raise
    except Exception as exc:  # 壊れた文書・ライブラリ内部エラーを恒久エラーに正規化する。
        raise _edit_error("edit_failed", str(exc)) from exc
    if len(out) > limit:
        raise _edit_error("too_large", f"編集結果が上限を超えています（最大 {limit} バイト）")

    results = [OpResult(op=op, applied=applied, warning=warning) for op, applied, warning in tuples]
    report = EditReport(
        applied_ops=sum(1 for r in results if r.applied > 0),
        results=results,
    )
    return EditResponse(data_base64=base64.b64encode(out).decode("ascii"), report=report)
