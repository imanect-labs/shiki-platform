"""POST /parse — Docling による構造化パース（日本語 OCR 対応）。

出力は「読み順の構造化ブロック列」（見出し・段落・表 Markdown・キャプション）。
チャンク化は Rust 側（crates/rag chunker）の責務で、worker はステートレスな
パース器に徹する。パース失敗は 422 の構造化エラーで返し、握りつぶさない。
"""

from __future__ import annotations

import asyncio
import logging
import threading
from io import BytesIO
from typing import Any

import httpx
from fastapi import APIRouter, HTTPException

from .schemas import BlockType, ParsedBlock, ParseRequest, ParseResponse
from .settings import get_settings

router = APIRouter()
logger = logging.getLogger(__name__)

# Docling が扱う MIME（Rust 側 indexer の対応 MIME 一覧と対にする）。
_DOCLING_TYPES = {
    "application/pdf",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    "text/html",
    "text/markdown",
    "text/csv",
}
_PLAIN_TEXT_TYPES = {"text/plain"}


class _ConverterHolder:
    """DocumentConverter の遅延シングルトン（レイアウトモデルのロードが重い）。"""

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._converter: Any | None = None

    def get(self) -> Any:
        if self._converter is None:
            with self._lock:
                if self._converter is None:
                    self._converter = _build_converter()
        return self._converter


def _build_converter() -> Any:
    from docling.datamodel.base_models import InputFormat
    from docling.datamodel.pipeline_options import (
        PdfPipelineOptions,
        TesseractCliOcrOptions,
    )
    from docling.document_converter import DocumentConverter, PdfFormatOption

    # スキャン PDF 向けに日本語 OCR（Tesseract CLI + jpn/jpn_vert）を有効化する。
    # Docling はテキスト層の無いビットマップ領域にのみ OCR を適用する。
    ocr = TesseractCliOcrOptions(lang=["jpn", "jpn_vert", "eng"])
    pdf_options = PdfPipelineOptions(do_ocr=True, do_table_structure=True, ocr_options=ocr)
    pdf_options.table_structure_options.do_cell_matching = True
    return DocumentConverter(
        format_options={InputFormat.PDF: PdfFormatOption(pipeline_options=pdf_options)}
    )


_holder = _ConverterHolder()


def get_converter_holder() -> _ConverterHolder:
    return _holder


def _parse_error(error: str, detail: str) -> HTTPException:
    return HTTPException(status_code=422, detail={"error": error, "detail": detail})


async def _download(url: str) -> bytes:
    limit = get_settings().max_download_bytes
    try:
        async with httpx.AsyncClient(timeout=60.0) as client:
            async with client.stream("GET", url) as resp:
                resp.raise_for_status()
                chunks: list[bytes] = []
                total = 0
                async for chunk in resp.aiter_bytes():
                    total += len(chunk)
                    if total > limit:
                        raise _parse_error(
                            "source_too_large", f"blob が上限 {limit} bytes を超えています"
                        )
                    chunks.append(chunk)
                return b"".join(chunks)
    except httpx.HTTPError as exc:
        raise _parse_error("source_fetch_failed", str(exc)) from exc


def _plain_text_blocks(data: bytes) -> list[ParsedBlock]:
    """text/plain は段落（空行区切り）に落とす。Docling を経由しない軽量経路。"""
    text = data.decode("utf-8", errors="replace")
    blocks = []
    for para in text.split("\n\n"):
        stripped = para.strip()
        if stripped:
            blocks.append(ParsedBlock(type=BlockType.PARAGRAPH, text=stripped))
    return blocks


def _page_of(item: Any) -> int | None:
    prov = getattr(item, "prov", None)
    if prov:
        return int(prov[0].page_no)
    return None


def _docling_blocks(document: Any) -> list[ParsedBlock]:
    """DoclingDocument を読み順の構造化ブロック列へ落とす。"""
    from docling_core.types.doc import DocItemLabel

    blocks: list[ParsedBlock] = []
    for item, _level in document.iterate_items():
        label = getattr(item, "label", None)
        if label in (DocItemLabel.PAGE_HEADER, DocItemLabel.PAGE_FOOTER):
            continue
        if label == DocItemLabel.TABLE:
            markdown = item.export_to_markdown(doc=document)
            if markdown.strip():
                blocks.append(
                    ParsedBlock(type=BlockType.TABLE, text=markdown, page=_page_of(item))
                )
            continue
        text = (getattr(item, "text", "") or "").strip()
        if not text:
            continue
        if label == DocItemLabel.TITLE:
            blocks.append(
                ParsedBlock(type=BlockType.HEADING, level=1, text=text, page=_page_of(item))
            )
        elif label == DocItemLabel.SECTION_HEADER:
            # SectionHeaderItem.level は 1 始まり。TITLE を 1 とし、その下に続ける。
            level = int(getattr(item, "level", 1)) + 1
            blocks.append(
                ParsedBlock(type=BlockType.HEADING, level=level, text=text, page=_page_of(item))
            )
        elif label == DocItemLabel.CAPTION:
            blocks.append(ParsedBlock(type=BlockType.CAPTION, text=text, page=_page_of(item)))
        else:
            # TEXT / PARAGRAPH / LIST_ITEM / CODE / FOOTNOTE などは段落として扱う。
            blocks.append(ParsedBlock(type=BlockType.PARAGRAPH, text=text, page=_page_of(item)))
    return blocks


@router.post("/parse")
async def parse(req: ParseRequest) -> ParseResponse:
    content_type = req.content_type.split(";")[0].strip().lower()
    data = await _download(req.source_url)

    if content_type in _PLAIN_TEXT_TYPES:
        return ParseResponse(blocks=_plain_text_blocks(data), used_ocr=False)

    if content_type not in _DOCLING_TYPES:
        raise _parse_error("unsupported_content_type", content_type)

    from docling.datamodel.base_models import ConversionStatus, DocumentStream

    logger.info("parse tenant=%s file=%s type=%s", req.tenant_id, req.file_name, content_type)
    converter = get_converter_holder().get()
    try:
        # Docling（OCR・表構造解析）は重い同期処理。イベントループをブロックすると
        # /healthz 含む全リクエストが止まるため、スレッドプールへ逃がす。
        result = await asyncio.to_thread(
            converter.convert,
            DocumentStream(name=req.file_name, stream=BytesIO(data)),
            raises_on_error=False,
        )
    except Exception as exc:  # Docling 内部の予期しない失敗も 422 に正規化する。
        raise _parse_error("parse_failed", str(exc)) from exc

    if result.status not in (ConversionStatus.SUCCESS, ConversionStatus.PARTIAL_SUCCESS):
        errors = "; ".join(str(e) for e in (result.errors or [])) or str(result.status)
        raise _parse_error("parse_failed", errors)

    blocks = _docling_blocks(result.document)
    if not blocks:
        raise _parse_error("empty_document", "抽出可能なテキストがありません")
    return ParseResponse(blocks=blocks, used_ocr=content_type == "application/pdf")
