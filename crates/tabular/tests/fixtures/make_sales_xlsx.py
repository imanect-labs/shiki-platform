"""sales.xlsx（表の上に表題の行がある Excel と、名前で読むシートの共通の材料・GAP-83・GAP-84）を作る。

使い方: python3 -I make_sales_xlsx.py sales.xlsx

Excel が保存する形に近い最小のブックを、標準ライブラリの zipfile だけで書く（中身を読めるように生成のしかたを残す）。

- 左端のシート「売上」: 1 行目に表題（A1 の「2024 年度 売上」）、2 行目の右に注記（C2 の「単位: 円」）、3 行目は空、
  4 行目が見出し（日付・品目・金額）、5〜7 行目が表。
- 2 つ目のシート「マスタ」（隠したシート）: 品目・区分。
- 3 つ目のシート「Data」: data と同じ名前（名前では読めない）。
- 4 つ目のシート「予算」: 区分・予算。
"""

import sys
import zipfile

MAIN = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
PKG = "http://schemas.openxmlformats.org/package/2006/relationships"

SHEETS = [
    # (名前, 状態)
    ("売上", ""),
    ("マスタ", "hidden"),
    ("Data", ""),
    ("予算", ""),
]

CONTENT_TYPES = (
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
    '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
    '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
    '<Default Extension="xml" ContentType="application/xml"/>'
    '<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>'
    + "".join(
        f'<Override PartName="/xl/worksheets/sheet{i}.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>'
        for i in range(1, len(SHEETS) + 1)
    )
    + '<Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/>'
    "</Types>"
)

ROOT_RELS = (
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
    f'<Relationships xmlns="{PKG}"><Relationship Id="rId1" Type="{REL}/officeDocument" Target="xl/workbook.xml"/></Relationships>'
)


def state(s):
    return f' state="{s}"' if s else ""


WORKBOOK = (
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
    f'<workbook xmlns="{MAIN}" xmlns:r="{REL}"><sheets>'
    + "".join(
        f'<sheet name="{name}" sheetId="{i}"{state(st)} r:id="rId{i}"/>'
        for i, (name, st) in enumerate(SHEETS, start=1)
    )
    + "</sheets></workbook>"
)

WORKBOOK_RELS = (
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
    f'<Relationships xmlns="{PKG}">'
    + "".join(
        f'<Relationship Id="rId{i}" Type="{REL}/worksheet" Target="worksheets/sheet{i}.xml"/>'
        for i in range(1, len(SHEETS) + 1)
    )
    + f'<Relationship Id="rId{len(SHEETS) + 1}" Type="{REL}/styles" Target="styles.xml"/>'
    "</Relationships>"
)

# cellXfs: 0 = 標準、1 = 日付（組み込みの書式 14）。
STYLES = (
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
    f'<styleSheet xmlns="{MAIN}"><fonts count="1"><font/></fonts><fills count="1"><fill/></fills>'
    '<borders count="1"><border/></borders><cellStyleXfs count="1"><xf/></cellStyleXfs>'
    '<cellXfs count="2"><xf numFmtId="0"/><xf numFmtId="14" applyNumberFormat="1"/></cellXfs></styleSheet>'
)


def text(ref, t):
    return f'<c r="{ref}" t="inlineStr"><is><t>{t}</t></is></c>'


def date(ref, serial):
    return f'<c r="{ref}" s="1"><v>{serial}</v></c>'


def num(ref, v):
    return f'<c r="{ref}"><v>{v}</v></c>'


def sheet(rows):
    body = "".join(f'<row r="{r}">{"".join(cells)}</row>' for r, cells in rows)
    return (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
        f'<worksheet xmlns="{MAIN}"><sheetData>{body}</sheetData></worksheet>'
    )


# 2024-04-01 は 45383（1900 年起点の通し日数）。
SALES = sheet(
    [
        (1, [text("A1", "2024 年度 売上")]),
        (2, [text("C2", "単位: 円")]),
        (4, [text("A4", "日付"), text("B4", "品目"), text("C4", "金額")]),
        (5, [date("A5", 45383), text("B5", "交通費"), num("C5", 1200)]),
        (6, [date("A6", 45384), text("B6", "会議費"), num("C6", 3000)]),
        (7, [date("A7", 45387), text("B7", "宿泊費"), num("C7", 9800)]),
    ]
)

MASTER = sheet(
    [
        (1, [text("A1", "品目"), text("B1", "区分")]),
        (2, [text("A2", "交通費"), text("B2", "経費")]),
        (3, [text("A3", "会議費"), text("B3", "経費")]),
        (4, [text("A4", "宿泊費"), text("B4", "出張")]),
    ]
)

SAME_AS_DATA = sheet([(1, [text("A1", "読めないシート")]), (2, [num("A2", 1)])])

BUDGET = sheet(
    [
        (1, [text("A1", "区分"), text("B1", "予算")]),
        (2, [text("A2", "経費"), num("B2", 5000)]),
        (3, [text("A3", "出張"), num("B3", 10000)]),
    ]
)


def main(path):
    parts = [
        ("[Content_Types].xml", CONTENT_TYPES),
        ("_rels/.rels", ROOT_RELS),
        ("xl/workbook.xml", WORKBOOK),
        ("xl/_rels/workbook.xml.rels", WORKBOOK_RELS),
        ("xl/styles.xml", STYLES),
        ("xl/worksheets/sheet1.xml", SALES),
        ("xl/worksheets/sheet2.xml", MASTER),
        ("xl/worksheets/sheet3.xml", SAME_AS_DATA),
        ("xl/worksheets/sheet4.xml", BUDGET),
    ]
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        for name, body in parts:
            info = zipfile.ZipInfo(name, date_time=(2024, 4, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            z.writestr(info, body.encode("utf-8"))


if __name__ == "__main__":
    main(sys.argv[1])
