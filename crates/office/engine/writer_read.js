// Shiki Office Engine — Writer の読み取り（#725）。
//
// 本文はブロック（段落・表）の並びとして扱う。ブロックの ID は「位置＋本文のハッシュ」
// （例 b12-3fa9）。人が同時に編集して位置がずれても、ハッシュで元のブロックを探し直せる。
// LLM 向けの整形は Rust 側（office::engine::writer::view）が持つ。

/** FNV-1a 32bit の先頭 16bit（4 桁の 16 進）。ブロックの同一性の確認用（暗号用途ではない）。 */
function shortHash(s) {
  let h = 0x811c9dc5;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 0x01000193) >>> 0;
  }
  return (h >>> 16).toString(16).padStart(4, "0");
}

function isTable(block) {
  return block.supportsService("com.sun.star.text.TextTable");
}

/** 表を 1 行の文字列にする（ハッシュとプレビュー用）。 */
function tableText(t) {
  const names = Array.from(t.getCellNames());
  return names.map((n) => t.getCellByName(n).getString()).join("|");
}

function blockKey(block) {
  return isTable(block) ? "T:" + tableText(block) : "P:" + block.getString();
}

/** 本文のブロックを並べる（ヘッダー・フッター・枠の中は含めない）。 */
function bodyBlocks() {
  const out = [];
  const en = DOC.getText().createEnumeration();
  while (en.hasMoreElements()) out.push(en.nextElement());
  return out;
}

function blockId(index, block) {
  return "b" + index + "-" + shortHash(blockKey(block));
}

/**
 * ID からブロックを探す。位置のハッシュが一致すればそれ、ずれていれば近くから同じハッシュを探す
 * （人が前に段落を足した場合など）。見つからなければ誤り（読み直しを促す）。
 */
function resolveBlock(blocks, id) {
  const m = /^b(\d+)-([0-9a-f]{4})$/.exec(id || "");
  if (!m) fail("bad_block_id", `ブロック ID「${id}」の形が不正です（office.read が返す b12-3fa9 の形）`);
  const index = parseInt(m[1], 10);
  const hash = m[2];
  if (index < blocks.length && shortHash(blockKey(blocks[index])) === hash) return index;
  for (let d = 1; d <= 50; d++) {
    for (const j of [index - d, index + d]) {
      if (j >= 0 && j < blocks.length && shortHash(blockKey(blocks[j])) === hash) return j;
    }
  }
  fail("stale_block_id", `ブロック ${id} が見つかりません（文書が変わった可能性があります）。office.read で読み直してください`);
}

const PARA_PROPS = [
  "ParaStyleName", "ParaAdjust", "ParaLeftMargin", "ParaFirstLineIndent", "ParaTopMargin",
  "ParaBottomMargin", "OutlineLevel", "NumberingLevel", "NumberingIsNumber", "ListLabelString",
  "BreakType", "ParaKeepTogether", "ParaBackColor",
];
const RUN_PROPS = [
  "CharWeight", "CharPosture", "CharUnderline", "CharStrikeout", "CharColor", "CharHeight",
  "CharFontName", "CharFontNameAsian", "CharBackColor", "CharEscapement", "HyperLinkURL",
];

function readProps(obj, names) {
  const out = {};
  for (const p of names) {
    try {
      const v = obj.getPropertyValue(p);
      out[p] = typeof v === "object" && v !== null ? enumName(v) : v;
    } catch (e) {
      // 種類によって持たないプロパティ。
    }
  }
  return out;
}

/** 直接付けた書式だけを読む（スタイル由来の値を含めると、書き戻したとき直接書式として焼き付く）。 */
function readDirectProps(obj, names) {
  const out = {};
  for (const p of names) {
    try {
      if (enumName(obj.getPropertyState(p)) !== "DIRECT_VALUE") continue;
      const v = obj.getPropertyValue(p);
      out[p] = typeof v === "object" && v !== null ? enumName(v) : v;
    } catch (e) {
      // 種類によって持たないプロパティ。
    }
  }
  return out;
}

function readParagraph(p, withRuns) {
  const out = { t: "p", text: p.getString(), props: readProps(p, PARA_PROPS) };
  // 目次などの索引の中の段落（読み取りでは 1 ブロックにまとめる）。
  try {
    if (p.getPropertyValue("DocumentIndex")) out.index = true;
  } catch (e) {
    // 索引の外。
  }
  if (withRuns) {
    out.runs = [];
    const en = p.createEnumeration();
    while (en.hasMoreElements()) {
      const r = en.nextElement();
      const kind = r.getPropertyValue("TextPortionType");
      if (kind !== "Text") {
        out.runs.push({ kind });
        continue;
      }
      out.runs.push({ text: r.getString(), props: readDirectProps(r, RUN_PROPS) });
    }
  }
  return out;
}

function readTable(t, withCells) {
  const names = Array.from(t.getCellNames());
  const out = {
    t: "table",
    name: t.getName(),
    rows: t.getRows().getCount(),
    cols: t.getColumns().getCount(),
    cells: names.length,
    text: tableText(t).slice(0, 200),
  };
  if (withCells) {
    out.cells = names.map((n) => {
      const c = t.getCellByName(n);
      const paras = [];
      const en = c.getText().createEnumeration();
      while (en.hasMoreElements()) {
        const p = en.nextElement();
        if (!isTable(p)) paras.push(readParagraph(p, true));
      }
      return {
        name: n,
        bg: c.getPropertyValue("BackColor"),
        row_span: (() => {
          try {
            return c.getPropertyValue("RowSpan");
          } catch (e) {
            return 1;
          }
        })(),
        paras,
      };
    });
    try {
      out.separators = Array.from(t.getPropertyValue("TableColumnSeparators")).map((s) => s.Position);
      out.rel_sum = t.getPropertyValue("TableColumnRelativeSum");
    } catch (e) {
      // 区切りが取れない表（結合が複雑など）。
    }
  }
  return out;
}

function pageInfo() {
  const out = {};
  try {
    const name = bodyBlocks()[0].getPropertyValue("PageDescName") || "Standard";
    const ps = DOC.getStyleFamilies().getByName("PageStyles").getByName(name);
    out.style = name;
    for (const p of ["Width", "Height", "IsLandscape", "LeftMargin", "RightMargin", "TopMargin", "BottomMargin", "HeaderIsOn", "FooterIsOn"]) {
      out[p] = ps.getPropertyValue(p);
    }
    if (out.HeaderIsOn) out.header = ps.getPropertyValue("HeaderText").getString();
    if (out.FooterIsOn) out.footer = ps.getPropertyValue("FooterText").getString();
  } catch (e) {
    // 先頭が表の文書などで PageDescName が無い場合は既定の扱い。
    try {
      const ps = DOC.getStyleFamilies().getByName("PageStyles").getByName("Standard");
      for (const p of ["Width", "Height", "IsLandscape", "LeftMargin", "RightMargin", "TopMargin", "BottomMargin", "HeaderIsOn", "FooterIsOn"]) {
        out[p] = ps.getPropertyValue(p);
      }
      out.style = "Standard";
    } catch (x) {
      out.style = null;
    }
  }
  return out;
}

/** よく使う段落スタイル（存在するものだけを一覧に出す）。 */
const COMMON_STYLES = [
  "Standard", "Text body", "Title", "Subtitle", "Heading 1", "Heading 2", "Heading 3", "Heading 4",
  "Quotations", "List Bullet", "List Number", "Caption", "Table Contents", "Footnote", "Signature",
];

/** 使える段落スタイル: 本文で使われているもの＋よく使うもの（[内部名, 表示名]）。 */
function availableStyles() {
  const fam = DOC.getStyleFamilies().getByName("ParagraphStyles");
  const names = new Set();
  for (const n of COMMON_STYLES) if (fam.hasByName(n)) names.add(n);
  for (const n of Array.from(fam.getElementNames())) {
    try {
      if (fam.getByName(n).isInUse() && !/^Contents|^Index|^Header|^Footer/.test(n)) names.add(n);
    } catch (e) {
      // 一覧の途中で消えたスタイル。
    }
  }
  return Array.from(names).slice(0, 40).map((n) => {
    let display = n;
    try {
      display = fam.getByName(n).getPropertyValue("DisplayName");
    } catch (e) {
      display = n;
    }
    return [n, display];
  });
}

/** 概要: ページ設定・ブロック一覧（プレビュー）・使用中のスタイル・目次の有無。 */
HANDLERS["writer.overview"] = (req) => {
  requireKind("writer");
  const blocks = bodyBlocks();
  const from = Math.max(0, req.from || 0);
  const to = Math.min(blocks.length, from + (req.limit || 200));
  const out = {
    count: blocks.length,
    from,
    blocks: [],
    page: pageInfo(),
    indexes: DOC.getDocumentIndexes().getCount(),
    styles: availableStyles(),
    tracking: DOC.getPropertyValue("RecordChanges"),
  };
  for (let i = from; i < to; i++) {
    STAGE = `ブロック ${i} の概要`;
    const b = blocks[i];
    const item = isTable(b) ? readTable(b, false) : readParagraph(b, false);
    item.id = blockId(i, b);
    if (item.t === "p" && item.text.length > 80) item.text = item.text.slice(0, 80) + "…";
    out.blocks.push(item);
  }
  return out;
};

/** 詳細: 指定したブロックの範囲を、ランと表のセルまで読む。 */
HANDLERS["writer.read_blocks"] = (req) => {
  requireKind("writer");
  const blocks = bodyBlocks();
  const a = resolveBlock(blocks, req.from);
  const b = req.to ? resolveBlock(blocks, req.to) : a;
  if (b < a) fail("bad_range", "範囲の終わりが始まりより前です");
  const out = { blocks: [] };
  for (let i = a; i <= Math.min(b, a + 100); i++) {
    STAGE = `ブロック ${i} の詳細`;
    const blk = blocks[i];
    const item = isTable(blk) ? readTable(blk, true) : readParagraph(blk, true);
    item.id = blockId(i, blk);
    out.blocks.push(item);
  }
  return out;
};
