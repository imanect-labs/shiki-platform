"""POST /parse — Docling による構造化パース（日本語 OCR 対応）。

出力は「読み順の構造化ブロック列」（見出し・段落・表 Markdown・キャプション）。
チャンク化は Rust 側（crates/rag chunker）の責務で、worker はステートレスな
パース器に徹する。パース失敗は 422 の構造化エラーで返し、握りつぶさない。
"""

from __future__ import annotations

import asyncio
import base64
import binascii
import json
import logging
import threading
from html.parser import HTMLParser
from io import BytesIO
from typing import Any

import httpx
from fastapi import APIRouter, HTTPException

from .schemas import BlockType, ParsedBlock, ParseRequest, ParseResponse, Prov
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


class ParseSlots:
    """同時に走る Docling 解析の有界カウンタ（スレッド安全）。

    Docling の `convert` は同期で**キャンセルできない**。`asyncio.to_thread` は待つのを
    やめるだけなので、呼び出し側（web_fetch は 90 秒）が諦めても解析スレッドは走り続ける。
    モデルが失敗後に別の文書を試すと、応答済みの解析がスレッドに残り続けて CPU と
    スレッドプールを食い潰す。よって**同時に走る解析数そのものを有界にする**。

    - 上限に達している間の要求は**待たせずに 503**（キューに積むと、既に諦められた
      クライアントぶんの仕事が延々と積み上がる＝有界にした意味が消える）。
    - 解放は**スレッド側の finally** で行う。ハンドラ側の finally だと、切断で await が
      中断された瞬間に「空いた」と誤認し、走り続けているスレッドを数え落とす。
    """

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._active = 0

    def try_acquire(self, limit: int) -> bool:
        with self._lock:
            if self._active >= limit:
                return False
            self._active += 1
            return True

    def release(self) -> None:
        with self._lock:
            self._active = max(0, self._active - 1)

    @property
    def active(self) -> int:
        with self._lock:
            return self._active


_slots = ParseSlots()


def get_parse_slots() -> ParseSlots:
    return _slots


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


def _decode_inline(content_base64: str) -> bytes:
    """インラインバイト列を取り出す（呼び出し側が取得済み・worker は取りに行かない）。

    上限はダウンロード経路と同じ値を使う（経路で緩まないようにする）。
    """
    limit = get_settings().max_download_bytes
    # base64 は 4/3 に膨らむ。デコード前に弾いてメモリを踏まない。
    if len(content_base64) > limit // 3 * 4 + 4:
        raise _parse_error("source_too_large", f"blob が上限 {limit} bytes を超えています")
    try:
        data = base64.b64decode(content_base64, validate=True)
    except (binascii.Error, ValueError) as exc:
        raise _parse_error("invalid_content", f"content_base64 が不正です: {exc}") from exc
    if len(data) > limit:
        raise _parse_error("source_too_large", f"blob が上限 {limit} bytes を超えています")
    return data


async def _load(req: ParseRequest) -> bytes:
    """要求からバイト列を得る。URL 指定のときだけ worker がダウンロードする。"""
    if req.content_base64 is not None:
        return _decode_inline(req.content_base64)
    # スキーマの検証で片方は必ず入っている。
    assert req.source_url is not None
    return await _download(req.source_url)


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
        "p",
        "div",
        "section",
        "li",
        "tr",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "blockquote",
        "figcaption",
        "br",
        "hr",
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


def _prov_of(item: Any) -> list[Prov]:
    """Docling の prov（ページ・bbox・charspan）をすべて写す（引用箇所を原本の上に描く・#508）。"""
    out: list[Prov] = []
    for p in getattr(item, "prov", None) or []:
        bbox = getattr(p, "bbox", None)
        if bbox is None:
            continue
        origin = str(getattr(bbox, "coord_origin", "BOTTOMLEFT")).upper()
        span = getattr(p, "charspan", None)
        out.append(
            Prov(
                page=int(p.page_no),
                bbox=(float(bbox.l), float(bbox.t), float(bbox.r), float(bbox.b)),
                origin="top_left" if "TOPLEFT" in origin else "bottom_left",
                charspan=(int(span[0]), int(span[1])) if span else None,
            )
        )
    return out


def _list_marker(item: Any, document: Any) -> str:
    """箇条書きの見た目の記号。Docling が記号を持たなければ、番号付きは兄弟内の順番から作る。"""
    marker = (getattr(item, "marker", "") or "").strip()
    if marker:
        return marker
    if not getattr(item, "enumerated", False):
        return "•"
    parent = getattr(item, "parent", None)
    try:
        group = parent.resolve(document) if parent is not None else None
        refs = [c.cref for c in getattr(group, "children", [])]
        return f"{refs.index(item.self_ref) + 1}."
    except (AttributeError, ValueError):
        return "•"


def _join_inline(parts: list[str]) -> str:
    """インラインの断片をつなぐ。英数字どうしの境目だけ空白を挟む（和文に空白を足さない）。"""
    out = ""
    for part in parts:
        if (
            out
            and part
            and out[-1].isascii()
            and out[-1].isalnum()
            and part[0].isascii()
            and part[0].isalnum()
        ):
            out += " "
        out += part
    return out


def _inline_text(group: Any, document: Any, seen: set[str]) -> tuple[str, list[Any]]:
    """inline グループ配下のテキストを読み順につなぐ（太字・リンクなどで分かれた断片）。"""
    parts: list[str] = []
    items: list[Any] = []
    for ref in getattr(group, "children", []) or []:
        child = ref.resolve(document)
        seen.add(child.self_ref)
        if getattr(child, "children", None) and not getattr(child, "text", ""):
            text, sub = _inline_text(child, document, seen)
            parts.append(text)
            items.extend(sub)
            continue
        parts.append((getattr(child, "text", "") or "").strip())
        items.append(child)
    return _join_inline([p for p in parts if p]), items


def _docling_blocks(document: Any) -> list[ParsedBlock]:
    """DoclingDocument を読み順の構造化ブロック列へ落とす。

    Docling は太字・リンクなどを含む段落を「inline グループ＋断片のテキスト群」として返す
    （装飾を含む箇条書きは、項目自身の本文が空で子に inline グループを持つ）。断片ごとに
    ブロックにすると 1 段落が何行にも割れるので、inline グループは 1 ブロックにまとめる。
    """
    from docling_core.types.doc import DocItemLabel, GroupLabel

    blocks: list[ParsedBlock] = []
    # inline グループとしてまとめ済みの要素（iterate_items が個別にも返すので飛ばす）。
    seen: set[str] = set()
    for item, _level in document.iterate_items(with_groups=True):
        if item.self_ref in seen:
            continue
        label = getattr(item, "label", None)
        if label == GroupLabel.INLINE:
            text, parts = _inline_text(item, document, seen)
            if not text:
                continue
            parent = item.parent.resolve(document) if item.parent is not None else None
            prov = [p for part in parts for p in _prov_of(part)]
            page = next((_page_of(part) for part in parts if _page_of(part) is not None), None)
            if getattr(parent, "label", None) == DocItemLabel.LIST_ITEM:
                blocks.append(
                    ParsedBlock(
                        type=BlockType.LIST_ITEM,
                        text=text,
                        page=page,
                        prov=prov,
                        list_marker=_list_marker(parent, document),
                    )
                )
            else:
                blocks.append(
                    ParsedBlock(type=BlockType.PARAGRAPH, text=text, page=page, prov=prov)
                )
            continue
        if not hasattr(item, "text") and label != DocItemLabel.TABLE:
            # その他のグループ（list / body など）は入れ物なので飛ばす。
            continue
        if label in (DocItemLabel.PAGE_HEADER, DocItemLabel.PAGE_FOOTER):
            continue
        page, prov = _page_of(item), _prov_of(item)
        if label == DocItemLabel.TABLE:
            markdown = item.export_to_markdown(doc=document)
            if markdown.strip():
                blocks.append(
                    ParsedBlock(type=BlockType.TABLE, text=markdown, page=page, prov=prov)
                )
            continue
        text = (getattr(item, "text", "") or "").strip()
        if not text:
            continue
        if label == DocItemLabel.TITLE:
            blocks.append(
                ParsedBlock(type=BlockType.HEADING, level=1, text=text, page=page, prov=prov)
            )
        elif label == DocItemLabel.SECTION_HEADER:
            # SectionHeaderItem.level は 1 始まり。TITLE を 1 とし、その下に続ける。
            level = int(getattr(item, "level", 1)) + 1
            blocks.append(
                ParsedBlock(type=BlockType.HEADING, level=level, text=text, page=page, prov=prov)
            )
        elif label == DocItemLabel.CAPTION:
            blocks.append(ParsedBlock(type=BlockType.CAPTION, text=text, page=page, prov=prov))
        elif label == DocItemLabel.LIST_ITEM:
            blocks.append(
                ParsedBlock(
                    type=BlockType.LIST_ITEM,
                    text=text,
                    page=page,
                    prov=prov,
                    list_marker=_list_marker(item, document),
                )
            )
        else:
            # TEXT / PARAGRAPH / CODE / FOOTNOTE などは段落として扱う。
            blocks.append(ParsedBlock(type=BlockType.PARAGRAPH, text=text, page=page, prov=prov))
    return blocks


@router.post("/parse")
async def parse(req: ParseRequest) -> ParseResponse:
    content_type = req.content_type.split(";")[0].strip().lower()
    data = await _load(req)

    if content_type in _PLAIN_TEXT_TYPES:
        return ParseResponse(blocks=_plain_text_blocks(data), used_ocr=False)

    if content_type == _SLIDE_TYPE:
        blocks = _slide_blocks(data)
        if not blocks:
            raise _parse_error("empty_document", "抽出可能なテキストがありません")
        return ParseResponse(blocks=blocks, used_ocr=False)

    if content_type not in _DOCLING_TYPES:
        raise _parse_error("unsupported_content_type", content_type)

    # 重い準備（モデルのロード）に入る前に枠を取る。取れなければ即 503。
    slots = get_parse_slots()
    limit = get_settings().max_concurrent_parses
    if not slots.try_acquire(limit):
        busy = f"解析の同時実行が上限 {limit} に達しています。時間を置いて再試行してください"
        raise HTTPException(
            status_code=503,
            detail={"error": "parser_busy", "detail": busy},
            headers={"Retry-After": "30"},
        )
    started = threading.Event()
    try:
        return await _docling_parse(req, data, content_type, started, slots)
    finally:
        # スレッドが起動していれば、解放は**スレッド側**（走り切った時点）で行う。
        # 起動前に失敗・切断した場合だけここで戻す（二重解放しない）。
        if not started.is_set():
            slots.release()


async def _docling_parse(
    req: ParseRequest,
    data: bytes,
    content_type: str,
    started: threading.Event,
    slots: ParseSlots,
) -> ParseResponse:
    """Docling 本体。枠（`slots`）は呼び出し側が取得済みで、解放はスレッド側で行う。"""
    from docling.datamodel.base_models import ConversionStatus, DocumentStream

    logger.info("parse tenant=%s file=%s type=%s", req.tenant_id, req.file_name, content_type)
    converter = get_converter_holder().get()

    def _convert() -> Any:
        started.set()
        try:
            return converter.convert(
                DocumentStream(name=req.file_name, stream=BytesIO(data)),
                raises_on_error=False,
            )
        finally:
            # 走り切った時点で解放する（呼び出し側が既に諦めていても同じ）。
            slots.release()

    try:
        # Docling（OCR・表構造解析）は重い同期処理。イベントループをブロックすると
        # /healthz 含む全リクエストが止まるため、スレッドプールへ逃がす。
        result = await asyncio.to_thread(_convert)
    except Exception as exc:  # Docling 内部の予期しない失敗も 422 に正規化する。
        raise _parse_error("parse_failed", str(exc)) from exc

    if result.status not in (ConversionStatus.SUCCESS, ConversionStatus.PARTIAL_SUCCESS):
        errors = "; ".join(str(e) for e in (result.errors or [])) or str(result.status)
        raise _parse_error("parse_failed", errors)

    blocks = _docling_blocks(result.document)
    if not blocks:
        raise _parse_error("empty_document", "抽出可能なテキストがありません")
    return ParseResponse(blocks=blocks, used_ocr=content_type == "application/pdf")
