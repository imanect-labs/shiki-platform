"""expenses.xlsx（CSV を集計で Excel を読むテストの共通の材料・D-17）を作る。

使い方: python3 -I make_expenses_xlsx.py expenses.xlsx

Excel が保存する形に近い最小のブックを、標準ライブラリの zipfile だけで書く（中身を読めるように生成のしかたを残す）。

- 左端のシート「明細」: 日付（日付の書式）・品目（共有の文字・ふりがな付き）・金額（数・1 つは保存された値つきの数式）・
  精算済み（真偽）・社員番号（文字の 007）。4 行目の後に空の行を 1 つ挟む。
- 2 つ目のシート「集計」: 左端でないシートは読まない（品目が「読まないシート」）。
"""

import sys
import zipfile

MAIN = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
PKG = "http://schemas.openxmlformats.org/package/2006/relationships"

CONTENT_TYPES = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
<Override PartName="/xl/worksheets/sheet2.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
<Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/>
<Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/>
</Types>"""

ROOT_RELS = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="{PKG}"><Relationship Id="rId1" Type="{REL}/officeDocument" Target="xl/workbook.xml"/></Relationships>"""

WORKBOOK = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="{MAIN}" xmlns:r="{REL}"><sheets>
<sheet name="明細" sheetId="1" r:id="rId1"/><sheet name="集計" sheetId="2" r:id="rId2"/>
</sheets></workbook>"""

WORKBOOK_RELS = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="{PKG}">
<Relationship Id="rId1" Type="{REL}/worksheet" Target="worksheets/sheet1.xml"/>
<Relationship Id="rId2" Type="{REL}/worksheet" Target="worksheets/sheet2.xml"/>
<Relationship Id="rId3" Type="{REL}/sharedStrings" Target="sharedStrings.xml"/>
<Relationship Id="rId4" Type="{REL}/styles" Target="styles.xml"/>
</Relationships>"""

# cellXfs: 0 = 標準、1 = 日付（組み込みの書式 14）。
STYLES = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="{MAIN}"><fonts count="1"><font/></fonts><fills count="1"><fill/></fills>
<borders count="1"><border/></borders><cellStyleXfs count="1"><xf/></cellStyleXfs>
<cellXfs count="2"><xf numFmtId="0"/><xf numFmtId="14" applyNumberFormat="1"/></cellXfs></styleSheet>"""

# 共有の文字（0 から）。交通費にはふりがな（rPh）を付ける（読むのは本文だけ）。
SHARED = [
    "日付",
    "品目",
    "金額",
    "精算済み",
    "社員番号",
    "交通費<rPh sb=\"0\" eb=\"3\"><t>コウツウヒ</t></rPh>",
    "会議費",
    "宿泊費",
    "読まないシート",
]


def shared_strings():
    items = []
    for s in SHARED:
        if "<rPh" in s:
            text, rest = s.split("<rPh", 1)
            items.append(f"<si><t>{text}</t><rPh{rest}</si>")
        else:
            items.append(f"<si><t>{s}</t></si>")
    return (
        f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
        f'<sst xmlns="{MAIN}" count="{len(SHARED)}" uniqueCount="{len(SHARED)}">{"".join(items)}</sst>'
    )


def s(ref, i):
    return f'<c r="{ref}" t="s"><v>{i}</v></c>'


def date(ref, serial):
    return f'<c r="{ref}" s="1"><v>{serial}</v></c>'


def num(ref, v):
    return f'<c r="{ref}"><v>{v}</v></c>'


def boolean(ref, v):
    return f'<c r="{ref}" t="b"><v>{1 if v else 0}</v></c>'


def inline(ref, text):
    return f'<c r="{ref}" t="inlineStr"><is><t>{text}</t></is></c>'


# 2024-04-01 は 45383（1900 年起点の通し日数）。
ROWS = [
    [s("A1", 0), s("B1", 1), s("C1", 2), s("D1", 3), s("E1", 4)],
    [date("A2", 45383), s("B2", 5), num("C2", 1200), boolean("D2", True), inline("E2", "007")],
    [date("A3", 45384), s("B3", 6), num("C3", 3000), boolean("D3", False), inline("E3", "012")],
    # 金額は数式（保存された値 800 を読む・計算はしない）。
    [date("A4", 45385), s("B4", 5), '<c r="C4"><f>400*2</f><v>800</v></c>', boolean("D4", True), inline("E4", "007")],
    None,
    [date("A6", 45387), s("B6", 7), num("C6", 9800), boolean("D6", False), inline("E6", "031")],
]


def sheet1():
    rows = []
    for i, cells in enumerate(ROWS, start=1):
        if cells is None:
            continue
        rows.append(f'<row r="{i}">{"".join(cells)}</row>')
    return (
        f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
        f'<worksheet xmlns="{MAIN}"><dimension ref="A1:E6"/><sheetData>{"".join(rows)}</sheetData></worksheet>'
    )


SHEET2 = (
    f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
    f'<worksheet xmlns="{MAIN}"><sheetData><row r="1">{s("A1", 1)}</row><row r="2">{s("A2", 8)}</row></sheetData></worksheet>'
)


def main(path):
    parts = [
        ("[Content_Types].xml", CONTENT_TYPES),
        ("_rels/.rels", ROOT_RELS),
        ("xl/workbook.xml", WORKBOOK),
        ("xl/_rels/workbook.xml.rels", WORKBOOK_RELS),
        ("xl/styles.xml", STYLES),
        ("xl/sharedStrings.xml", shared_strings()),
        ("xl/worksheets/sheet1.xml", sheet1()),
        ("xl/worksheets/sheet2.xml", SHEET2),
    ]
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        for name, body in parts:
            info = zipfile.ZipInfo(name, date_time=(2024, 4, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            z.writestr(info, body.encode("utf-8"))


if __name__ == "__main__":
    main(sys.argv[1])
