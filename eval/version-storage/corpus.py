"""Deterministic, generated edit histories; no customer documents or edit traces."""

import hashlib
import io
import random
import re
import zipfile
from dataclasses import dataclass
from pathlib import Path
from xml.sax.saxutils import escape

import numpy as np
from docx import Document
from openpyxl import Workbook, load_workbook
from PIL import Image, PngImagePlugin
from pptx import Presentation
from reportlab.pdfgen import canvas
from reportlab.lib.utils import ImageReader

ROOT = Path(__file__).resolve().parents[2]
SEEDS = (73, 941)
EDITS = ("noop", "local", "head", "append", "delete", "scattered", "mass", "rewrite", "undo")


@dataclass
class Case:
    name: str
    family: str
    source: str
    edit: str
    seed: int
    versions: list[bytes]


def digest(data):
    return hashlib.sha256(data).hexdigest()


def text_base(kind, size, seed):
    rng = random.Random(seed)
    lines = []
    used = 0
    while used < size:
        n = len(lines)
        v = rng.randrange(10**9)
        if kind == "markdown":
            line = f"## 議事録 {n}\n担当部門の計画と検証結果 {v}。次回までに確認する。\n"
        elif kind == "code":
            line = f"export const item_{n} = {{ id: {v}, label: 'record {n}', enabled: true }};\n"
        elif kind == "csv":
            line = f'{n},部署{n % 23},商品{v},2026-11-12,{v % 10000},確認済み\n'
        elif kind == "json":
            line = f'{{"id":{n},"value":{v},"name":"記録{n}","active":true}}\n'
        else:
            line = f'<p id="p{n}" class="record">項目 {v}: 担当部署の説明文です。</p>\n'
        lines.append(line)
        used += len(line.encode())
    return "".join(lines).encode()


def mutate(base, edit, index, rng, original):
    """Raw byte controls deliberately include arbitrary binary insertion/deletion."""
    n = len(base)
    if edit == "noop":
        return base
    if edit == "undo" and index % 4 == 0:
        return original
    if edit == "rewrite":
        return rng.randbytes(n)
    if edit == "head":
        return f"inserted-{index:04d}\n".encode() + base
    if edit == "append":
        return base + (f"new record {index:04d} 記録\n".encode() * 8)
    if edit == "delete":
        pos = rng.randrange(max(1, n - 80))
        return base[:pos] + base[pos + 64:]
    count = 16 if edit == "scattered" else 1
    length = max(1, n // 4) if edit == "mass" else min(24, n)
    result = bytearray(base)
    for _ in range(count):
        pos = rng.randrange(max(1, len(result) - length + 1))
        result[pos:pos + length] = rng.randbytes(length)
    return bytes(result)


def text_history(kind, size, edit, seed, depth):
    rng = random.Random(seed)
    original = text_base(kind, size, seed)
    out = [original]
    for i in range(1, depth):
        if edit == "rewrite":
            out.append(text_base(kind, size, seed + i))
        else:
            out.append(mutate(out[-1], edit, i, rng, original))
    return out


def archive(entries, index=0, repack=False):
    """Preserve exact ZIP bytes as generated; vary metadata/order in repack controls."""
    out = io.BytesIO()
    with zipfile.ZipFile(out, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=6) as z:
        items = list(entries.items())
        if repack and index % 2:
            items.reverse()
        for name, data in items:
            if name == "docProps/core.xml":
                # openpyxl overwrites modified time on every save. Remove clock noise
                # from generated packages; checked-in first versions stay exact.
                data = re.sub(rb"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z",
                              b"2026-11-12T00:00:00Z", data)
            date = (2026, 11, 12, 0, index // 60 % 60, index % 30 * 2) if repack else (2026, 1, 1, 0, 0, 0)
            info = zipfile.ZipInfo(name, date_time=date)
            info.compress_type = zipfile.ZIP_DEFLATED
            z.writestr(info, data)
    return out.getvalue()


def office_entries(kind, scale, seed):
    """Use real Office libraries for the initial package and XML for controlled edits."""
    out = io.BytesIO()
    if kind == "docx":
        doc = Document()
        for line in text_base("markdown", scale, seed).decode().splitlines():
            doc.add_paragraph(line)
        doc.save(out)
    elif kind == "xlsx":
        book = Workbook()
        sheet = book.active
        for i, line in enumerate(text_base("csv", scale, seed).decode().splitlines()):
            sheet.append([i, line, i * 7, f"=A{i + 1}+C{i + 1}"])
        book.save(out)
    else:
        deck = Presentation()
        lines = text_base("markdown", scale, seed).decode().splitlines()
        for start in range(0, len(lines), 50):
            slide = deck.slides.add_slide(deck.slide_layouts[1])
            slide.shapes.title.text = f"計画 {start}"
            slide.placeholders[1].text = "\n".join(lines[start:start + 50])
        deck.save(out)
    with zipfile.ZipFile(io.BytesIO(out.getvalue())) as z:
        return {name: z.read(name) for name in z.namelist()}


def office_history(entries, kind, edit, seed, depth):
    rng = random.Random(seed)
    original = dict(entries)
    targets = {"docx": "word/document.xml", "xlsx": "xl/worksheets/sheet1.xml", "pptx": "ppt/slides/slide1.xml"}
    target = targets[kind]
    out = [archive(entries)]
    for i in range(1, depth):
        if edit == "noop":
            out.append(out[-1])
            continue
        if edit == "undo" and i % 4 == 0:
            entries = dict(original)
            out.append(archive(entries))
            continue
        entries = dict(entries)
        if edit != "repack":
            # A comment is valid in OOXML and gives controlled, reversible byte edits.
            # Content replacements below also change visible text for local/rewrite.
            xml = entries[target].decode()
            if edit in ("local", "undo"):
                xml = re.sub(r"[0-9]{6,9}", f"{rng.randrange(10**8, 10**9)}", xml, count=1)
            elif edit == "rewrite":
                xml = re.sub(r"[0-9]{6,9}", lambda _: str(rng.randrange(10**8, 10**9)), xml)
            else:
                pos = xml.index(">") + 1
                xml = xml[:pos] + f"<!-- {edit} revision {i} {escape(str(rng.random()))} -->" + xml[pos:]
            entries[target] = xml.encode()
            if edit == "structure":
                entries[f"benchmark/part-{i}.txt"] = rng.randbytes(1024)
        out.append(archive(entries, i, repack=edit == "repack"))
    return out


def media_history(kind, size, edit, seed, depth):
    rng = random.Random(seed)
    if kind == "random":
        original = rng.randbytes(size)
        out = [original]
        for i in range(1, depth):
            op = "append" if edit == "metadata" else edit
            out.append(mutate(out[-1], op, i, rng, original))
        return out
    out = []
    side = 128 if size < 100_000 else 512
    pixels = np.random.default_rng(seed).integers(0, 256, (side, side, 3), dtype=np.uint8)
    original = pixels.copy()
    for i in range(depth):
        if edit == "undo" and i % 4 == 0:
            pixels = original.copy()
        elif edit == "rewrite":
            pixels = np.random.default_rng(seed + i).integers(0, 256, pixels.shape, dtype=np.uint8)
        elif edit != "metadata" and i:
            pixels[side // 2:side // 2 + 8, side // 2:side // 2 + 8] = i % 256
        dest = io.BytesIO()
        if kind == "pdf":
            c = canvas.Canvas(dest, invariant=1, pageCompression=1)
            for page in range(2 if size < 100_000 else 20):
                c.drawString(30, 800, f"Report page {page}, revision {i if edit != 'undo' or i % 4 else 0}")
                c.drawImage(ImageReader(Image.fromarray(pixels)), 30, 200, width=400, height=400)
                c.showPage()
            c.save()
        elif kind == "png":
            meta = PngImagePlugin.PngInfo()
            meta.add_text("Comment", f"revision {i}" if edit == "metadata" else "fixed")
            Image.fromarray(pixels).save(dest, format="PNG", pnginfo=meta)
        else:
            Image.fromarray(pixels).save(dest, format="JPEG", quality=90, comment=f"revision {i}".encode() if edit == "metadata" else b"fixed")
        out.append(dest.getvalue())
    return out


def serialized_history(kind, original, edit, seed, depth):
    """Actual library load/edit/save, including visible additions and deletions."""
    out = [archive(original)]
    rng = random.Random(seed)
    for i in range(1, depth):
        source = io.BytesIO(out[-1])
        dest = io.BytesIO()
        label = f"編集 {i} 検証値 {rng.randrange(10**9)}"
        if kind == "docx":
            doc = Document(source)
            if edit == "add_delete":
                if i % 3 == 0 and len(doc.paragraphs) > 1:
                    p = doc.paragraphs[-1]._element
                    p.getparent().remove(p)
                else:
                    doc.add_paragraph(label)
            elif edit == "global":
                for p in doc.paragraphs:
                    p.text = p.text.replace("確認", f"確認{i}")
            else:
                doc.paragraphs[len(doc.paragraphs) // 2].text = label
            doc.save(dest)
        elif kind == "xlsx":
            book = load_workbook(source)
            sheet = book.active
            if edit == "add_delete":
                if i % 3 == 0:
                    sheet.delete_rows(2)
                else:
                    sheet.insert_rows(2)
                    sheet.cell(2, 2, label)
            elif edit == "global":
                for row in sheet:
                    row[1].value = label
            else:
                sheet.cell(max(1, sheet.max_row // 2), 2, label)
            book.save(dest)
        else:
            deck = Presentation(source)
            if edit == "add_delete":
                # Slide insertions change relationships and content types as well as content.
                slide = deck.slides.add_slide(deck.slide_layouts[1])
                slide.shapes.title.text = label
            elif edit == "global":
                for slide in deck.slides:
                    slide.shapes.title.text = label
            else:
                deck.slides[len(deck.slides) // 2].shapes.title.text = label
            deck.save(dest)
        # Freeze archive timestamps; preserve library-produced entry order/content.
        with zipfile.ZipFile(io.BytesIO(dest.getvalue())) as z:
            out.append(archive({n: z.read(n) for n in z.namelist()}))
    return out


def cases(quick=False, wanted=lambda name: True):
    seeds = SEEDS[:1] if quick else SEEDS
    sizes = (16_384,) if quick else (16_384, 262_144, 1_048_576)
    depth = 9 if quick else 17
    for kind in ("markdown", "code", "csv", "json", "html"):
        for size in sizes:
            for seed in seeds:
                for edit in EDITS:
                    name = f"{kind}-{size}-{edit}-{seed}"
                    if not wanted(name):
                        continue
                    yield Case(name, kind, "generated-text", edit, seed,
                               text_history(kind, size, edit, seed, depth))
    for kind in ("random", "png", "jpeg", "pdf"):
        for size in ((65_536,) if quick else (65_536, 524_288)):
            for seed in seeds:
                for edit in ("metadata", "local", "rewrite", "undo"):
                    name = f"{kind}-{size}-{edit}-{seed}"
                    if not wanted(name):
                        continue
                    yield Case(name, kind, "generated-media", edit, seed,
                               media_history(kind, size, edit, seed, depth))
    for kind in ("docx", "xlsx", "pptx"):
        for size in ((16_384,) if quick else (16_384, 262_144)):
            for seed in seeds:
                group = [f"{kind}-{size}-{e}-{seed}" for e in ("noop", "local", "head", "structure", "repack", "rewrite", "undo")]
                group += [f"serializer-{kind}-{size}-{e}-{seed}" for e in ("local", "add_delete", "global")]
                if not any(wanted(n) for n in group):
                    continue
                entries = office_entries(kind, size, seed)
                for edit in ("noop", "local", "head", "structure", "repack", "rewrite", "undo"):
                    name = f"{kind}-{size}-{edit}-{seed}"
                    if not wanted(name):
                        continue
                    yield Case(name, kind, "generated-ooxml", edit, seed,
                               office_history(entries, kind, edit, seed, depth))
                for edit in ("local", "add_delete", "global"):
                    name = f"serializer-{kind}-{size}-{edit}-{seed}"
                    if not wanted(name):
                        continue
                    yield Case(name, kind, "library-load-edit-save", edit, seed,
                               serialized_history(kind, entries, edit, seed, depth))
    fixtures = ("ingestion-worker/tests/fixtures/sample.docx", "ingestion-worker/tests/fixtures/sample.xlsx",
                "crates/office/templates/blank.docx", "crates/office/templates/blank.xlsx",
                "web/e2e/fixtures/office-live-demo.xlsx")
    for path in fixtures:
        prefix = f"fixture-{path.replace('/', '_')}"
        if not any(wanted(f"{prefix}-{e}") for e in ("local", "repack", "undo")):
            continue
        raw = (ROOT / path).read_bytes()
        kind = Path(path).suffix[1:]
        with zipfile.ZipFile(io.BytesIO(raw)) as z:
            entries = {n: z.read(n) for n in z.namelist()}
        for edit in ("local", "repack", "undo"):
            if not wanted(f"{prefix}-{edit}"):
                continue
            versions = office_history(entries, kind, edit, 73, depth)
            versions[0] = raw  # Include the exact checked-in bytes, not just the rebuilt package.
            yield Case(f"fixture-{path.replace('/', '_')}-{edit}", kind, path, edit, 73, versions)
    if not quick:
        for kind in ("markdown", "csv", "random", "docx"):
            for seed in seeds:
                for edit in ("local", "head"):
                    if not wanted(f"long-{kind}-{edit}-{seed}"):
                        continue
                    if kind == "docx":
                        versions = office_history(office_entries(kind, 65_536, seed), kind, edit, seed, 129)
                    elif kind == "random":
                        rng = random.Random(seed)
                        original = rng.randbytes(65_536)
                        versions = [original]
                        for i in range(1, 129):
                            versions.append(mutate(versions[-1], edit, i, rng, original))
                    else:
                        versions = text_history(kind, 65_536, edit, seed, 129)
                    yield Case(f"long-{kind}-{edit}-{seed}", kind, "generated-long-history", edit, seed, versions)
        for kind in ("code", "random"):
            for edit in ("local", "append", "rewrite"):
                if not wanted(f"large-{kind}-{edit}"):
                    continue
                versions = (text_history(kind, 4_194_304, edit, 73, 9) if kind == "code" else
                            media_history(kind, 4_194_304, edit, 73, 9))
                yield Case(f"large-{kind}-{edit}", kind, "generated-4MiB", edit, 73, versions)
