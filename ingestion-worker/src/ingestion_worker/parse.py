"""POST /parse — Docling による構造化パース（日本語 OCR 対応）。

出力は「読み順の構造化ブロック列」（見出し・段落・表 Markdown・キャプション）。
チャンク化は Rust 側（crates/rag chunker）の責務で、worker はステートレスな
パース器に徹する。パース失敗は 422 の構造化エラーで返し、握りつぶさない。
"""

from __future__ import annotations

import asyncio
import json
import logging
import threading
from html.parser import HTMLParser
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
# shiki スライド（Task 11.1・design §4.8.3）。JSON からスライド順にテキストを抽出する
# 軽量経路（Docling を経由しない）。page = スライド番号として引用位置を保つ。
_SLIDE_TYPE = "application/vnd.shiki.slide+json"


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


class _HtmlTextExtractor(HTMLParser):
    """スライド HTML からブロック単位のテキストを抽出する（タグは信頼しない・文字のみ拾う）。

    script/style の中身は**テキストとして拾わない**: 直接アップロード等で正規化前の
    `.slide` が来た場合に、実行コードや隠しペイロードを RAG の検索対象へ混入させない
    （プロンプト注入面の縮小・レビュー指摘対応）。
    """

    _BLOCK_TAGS = {
        "p", "div", "section", "li", "tr", "h1", "h2", "h3", "h4", "h5", "h6",
        "blockquote", "figcaption", "br", "hr",
    }
    _SKIP_TAGS = {"script", "style", "template", "noscript"}

    def __init__(self) -> None:
        super().__init__()
        self.parts: list[str] = []
        self._current: list[str] = []
        self._skip_depth = 0

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        if tag in self._SKIP_TAGS:
            self._skip_depth += 1
            return
        if tag in self._BLOCK_TAGS:
            self._flush()

    def handle_endtag(self, tag: str) -> None:
        if tag in self._SKIP_TAGS:
            self._skip_depth = max(0, self._skip_depth - 1)
            return
        if tag in self._BLOCK_TAGS:
            self._flush()

    def handle_data(self, data: str) -> None:
        if self._skip_depth > 0:
            return
        self._current.append(data)

    def _flush(self) -> None:
        text = "".join(self._current).strip()
        if text:
            self.parts.append(text)
        self._current = []

    def close(self) -> None:  # noqa: D102 - HTMLParser の終端で残りを flush する。
        super().close()
        self._flush()


def _html_text_parts(html: str) -> list[str]:
    extractor = _HtmlTextExtractor()
    extractor.feed(html)
    extractor.close()
    return extractor.parts


def _slide_blocks(data: bytes) -> list[ParsedBlock]:
    """`.slide`（正規化 JSON）をスライド順の見出し＋段落ブロックへ落とす。"""
    try:
        doc = json.loads(data.decode("utf-8", errors="replace"))
    except json.JSONDecodeError as exc:
        raise _parse_error("parse_failed", f"不正なスライド JSON: {exc}") from exc
    if not isinstance(doc, dict):
        raise _parse_error("parse_failed", "スライド JSON はオブジェクトである必要があります")

    blocks: list[ParsedBlock] = []
    meta = doc.get("meta") or {}
    title = meta.get("title") if isinstance(meta, dict) else None
    if isinstance(title, str) and title.strip():
        blocks.append(ParsedBlock(type=BlockType.HEADING, level=1, text=title.strip()))

    slides = doc.get("slides") or []
    if not isinstance(slides, list):
        slides = []
    for page, slide in enumerate(slides, start=1):
        if not isinstance(slide, dict):
            continue
        parts = _html_text_parts(str(slide.get("html") or ""))
        heading = parts[0] if parts else f"スライド {page}"
        blocks.append(ParsedBlock(type=BlockType.HEADING, level=2, text=heading, page=page))
        for part in parts[1:]:
            blocks.append(ParsedBlock(type=BlockType.PARAGRAPH, text=part, page=page))
        notes = str(slide.get("notes") or "").strip()
        if notes:
            blocks.append(ParsedBlock(type=BlockType.PARAGRAPH, text=notes, page=page))
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

    if content_type == _SLIDE_TYPE:
        blocks = _slide_blocks(data)
        if not blocks:
            raise _parse_error("empty_document", "抽出可能なテキストがありません")
        return ParseResponse(blocks=blocks, used_ocr=False)

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
