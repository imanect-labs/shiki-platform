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

/**
 * 直接付けた書式だけを読む（スタイル由来の値を含めると、書き戻したとき直接書式として焼き付く）。
 * style を渡すと、スタイルと同じ値の直接書式（docx の取り込みが付ける既定値など）は省く。
 */
function readDirectProps(obj, names, style) {
  const out = {};
  for (const p of names) {
    try {
      if (enumName(obj.getPropertyState(p)) !== "DIRECT_VALUE") continue;
      const v = obj.getPropertyValue(p);
      if (style && sameValue(v, style.getPropertyValue(p))) continue;
      out[p] = typeof v === "object" && v !== null ? enumName(v) : v;
    } catch (e) {
      // 種類によって持たないプロパティ。
    }
  }
  return out;
}

/** 文書の変更履歴（[{type, start, end}]）。種類は "Insert" / "Delete" / "Format" など。 */
function redlineSpans() {
  const out = [];
  try {
    const en = DOC.getRedlines().createEnumeration();
    while (en.hasMoreElements()) {
      const r = en.nextElement();
      try {
        out.push({ type: r.getPropertyValue("RedlineType"), start: r.getPropertyValue("RedlineStart"), end: r.getPropertyValue("RedlineEnd") });
      } catch (e) {
        // 位置の取れない変更（表の行の変更など）。
      }
    }
  } catch (e) {
    // 変更履歴を持たない文書。
  }
  return out;
}

/** range が変更履歴 red の中にあるか（別の本文＝セルやヘッダーにあれば比べられないので false）。 */
function insideRedline(range, red) {
  try {
    const t = range.getText();
    return t.compareRegionStarts(red.start, range) >= 0 && t.compareRegionEnds(range, red.end) >= 0;
  } catch (e) {
    return false;
  }
}

/** range と変更履歴 red が重なるか。 */
function touchesRedline(range, red) {
  try {
    const t = range.getText();
    return t.compareRegionStarts(red.start, range.getEnd()) > 0 && t.compareRegionEnds(range.getStart(), red.end) > 0;
  } catch (e) {
    return false;
  }
}

function sameValue(a, b) {
  if (a === b) return true;
  if (a && b && typeof a === "object" && typeof b === "object") {
    if ("Mode" in a && "Height" in a) return a.Mode === b.Mode && a.Height === b.Height;
    return enumName(a) === enumName(b);
  }
  return false;
}

function paragraphStyleOf(p) {
  try {
    return DOC.getStyleFamilies().getByName("ParagraphStyles").getByName(p.getPropertyValue("ParaStyleName"));
  } catch (e) {
    return null;
  }
}

function markedText(p, reds) {
  let text = "";
  try {
    const en = p.createEnumeration();
    while (en.hasMoreElements()) {
      const r = en.nextElement();
      if (r.getPropertyValue("TextPortionType") !== "Text") continue;
      const t = r.getString();
      const red = reds.find((x) => (x.type === "Insert" || x.type === "Delete") && insideRedline(r, x));
      text += red ? (red.type === "Insert" ? `［＋${t}］` : `［－${t}］`) : t;
    }
  } catch (e) {
    return p.getString();
  }
  return text;
}

function readParagraph(p, withRuns, reds) {
  const out = { t: "p", text: p.getString(), props: readProps(p, PARA_PROPS) };
  if (reds && reds.length && reds.some((r) => touchesRedline(p, r))) {
    out.changes = true;
    // 概要の文字列は、挿入を［＋…］、削除を［－…］で示す（連結すると読めないため）。
    if (!withRuns) out.text = markedText(p, reds);
  }
  // 目次などの索引の中の段落（読み取りでは 1 ブロックにまとめる）。
  try {
    if (p.getPropertyValue("DocumentIndex")) out.index = true;
  } catch (e) {
    // 索引の外。
  }
  if (withRuns) {
    const style = paragraphStyleOf(p);
    out.direct = readDirectProps(p, ["ParaTopMargin", "ParaBottomMargin"], style);
    const ls = p.getPropertyValue("ParaLineSpacing");
    if (enumName(p.getPropertyState("ParaLineSpacing")) === "DIRECT_VALUE" && !(style && sameValue(ls, style.getPropertyValue("ParaLineSpacing")))) {
      out.direct.LineSpacingMode = ls.Mode;
      out.direct.LineSpacingHeight = ls.Height;
    }
    out.runs = [];
    const paraChars = readDirectProps(p, RUN_PROPS, style);
    const en = p.createEnumeration();
    while (en.hasMoreElements()) {
      const r = en.nextElement();
      const kind = r.getPropertyValue("TextPortionType");
      if (kind !== "Text") {
        out.runs.push({ kind });
        continue;
      }
      // 段落全体に付いた文字の書式（取り込み・docx の段落記号の書式）もランの書式として見せる。
      const run = { text: r.getString(), props: Object.assign({}, paraChars, readDirectProps(r, RUN_PROPS, style)) };
      if (reds && reds.length) {
        const red = reds.find((x) => (x.type === "Insert" || x.type === "Delete") && insideRedline(r, x));
        if (red) run.redline = red.type;
      }
      out.runs.push(run);
    }
  }
  return out;
}

function readTable(t, withCells, reds) {
  const names = Array.from(t.getCellNames());
  const out = {
    t: "table",
    name: t.getName(),
    rows: t.getRows().getCount(),
    cols: tableShape(t).cols,
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
        if (!isTable(p)) paras.push(readParagraph(p, true, reds));
      }
      return {
        name: n,
        bg: c.getPropertyValue("BackColor"),
        valign: c.getPropertyValue("VertOrient"),
        borders: ["TopBorder", "BottomBorder", "LeftBorder", "RightBorder"].map((k) => {
          const b = c.getPropertyValue(k);
          const w = b.LineWidth || b.OuterLineWidth + b.InnerLineWidth + b.LineDistance;
          return w > 0 ? [w, b.Color, b.LineStyle] : 0;
        }),
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
      out.width = t.getPropertyValue("Width");
      out.hori = t.getPropertyValue("HoriOrient");
    } catch (e) {
      // 区切りが取れない表（結合が複雑など）。
    }
  }
  return out;
}

/** ヘッダー・フッターの文字列。ページ番号などのフィールドは書き込みと同じ &[Page] の形で返す。 */
const FIELD_CODES = [
  ["PageNumber", "&[Page]"], ["PageCount", "&[Pages]"], ["DateTime", "&[Date]"], ["DocInfo.Title", "&[Title]"],
  ["FileName", "&[File]"],
];

function headerFooterText(text) {
  const parts = [];
  try {
    const en = text.createEnumeration();
    while (en.hasMoreElements()) {
      const p = en.nextElement();
      if (isTable(p)) continue;
      let line = "";
      const pe = p.createEnumeration();
      while (pe.hasMoreElements()) {
        const r = pe.nextElement();
        const kind = r.getPropertyValue("TextPortionType");
        if (kind === "TextField") {
          const f = r.getPropertyValue("TextField");
          const hit = FIELD_CODES.find(([svc]) => f.supportsService("com.sun.star.text.TextField." + svc));
          line += hit ? hit[1] : r.getString();
        } else {
          line += r.getString();
        }
      }
      parts.push(line);
    }
  } catch (e) {
    return text.getString();
  }
  return parts.join(" / ");
}

/** 表紙（1 ページ目だけヘッダー・フッターを出さない）に使うページスタイル。 */
const TITLE_PAGE_STYLE = "First Page";

/**
 * 本文のページスタイル（用紙・余白・ヘッダー/フッターの対象）と、表紙が有効か。
 * 先頭のブロックが表紙のページスタイルなら、その次のスタイル（FollowStyle）が本文。
 */
function bodyPageStyle() {
  const fam = DOC.getStyleFamilies().getByName("PageStyles");
  let name = "Standard";
  try {
    name = bodyBlocks()[0].getPropertyValue("PageDescName") || "Standard";
  } catch (e) {
    // 先頭が表などで PageDescName を持たない文書。
  }
  if (!fam.hasByName(name)) name = "Standard";
  let style = fam.getByName(name);
  let titlePage = false;
  if (name === TITLE_PAGE_STYLE) {
    const follow = style.getPropertyValue("FollowStyle");
    if (follow && follow !== name && fam.hasByName(follow)) {
      name = follow;
      style = fam.getByName(follow);
      titlePage = true;
    }
  }
  return { name, style, titlePage };
}

function pageInfo() {
  const out = {};
  try {
    const { name, style: ps, titlePage } = bodyPageStyle();
    out.style = name;
    out.title_page = titlePage;
    for (const p of ["Width", "Height", "IsLandscape", "LeftMargin", "RightMargin", "TopMargin", "BottomMargin", "HeaderIsOn", "FooterIsOn"]) {
      out[p] = ps.getPropertyValue(p);
    }
    if (out.HeaderIsOn) out.header = headerFooterText(ps.getPropertyValue("HeaderText"));
    if (out.FooterIsOn) out.footer = headerFooterText(ps.getPropertyValue("FooterText"));
  } catch (e) {
    out.style = null;
  }
  return out;
}

const IMPORT_ARTIFACT_STYLES = new Set(["表の内容", "表の見出し", "索引", "ヘッダーとフッター"]);

/** よく使う段落スタイル（存在するものだけを一覧に出す）。 */
const COMMON_STYLES = [
  "Standard", "Title", "Subtitle", "Heading 1", "Heading 2", "Heading 3", "Heading 4",
  "Quotations", "List Bullet", "List Number", "Caption", "Table Contents", "Footnote", "Signature",
];

/** 使える段落スタイル: 本文で使われているもの＋よく使うもの（[内部名, 表示名]）。 */
function availableStyles() {
  const fam = DOC.getStyleFamilies().getByName("ParagraphStyles");
  const names = new Set();
  for (const n of COMMON_STYLES) if (fam.hasByName(n)) names.add(n);
  for (const n of Array.from(fam.getElementNames())) {
    try {
      // docx を往復すると LibreOffice の組み込みスタイルが日本語名の独自スタイルとして残る（結合セルの隠れた段落などが使う）。
      if (fam.getByName(n).isInUse() && !/^Contents|^Index|^Header|^Footer/.test(n) && !IMPORT_ARTIFACT_STYLES.has(n)) names.add(n);
    } catch (e) {
      // 一覧の途中で消えたスタイル。
    }
  }
  return Array.from(names).slice(0, 40).map((n) => {
    let display = n;
    let look = null;
    try {
      const st = fam.getByName(n);
      display = st.getPropertyValue("DisplayName");
      look = {
        size: st.getPropertyValue("CharHeight"),
        bold: st.getPropertyValue("CharWeight") > 100,
        color: st.getPropertyValue("CharColor"),
        font: st.getPropertyValue("CharFontNameAsian") || st.getPropertyValue("CharFontName"),
      };
    } catch (e) {
      display = n;
    }
    return [n, display, look];
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
    pages: (() => {
      try {
        return DOC.getCurrentController().getPropertyValue("PageCount");
      } catch (e) {
        return null;
      }
    })(),
  };
  const reds = redlineSpans();
  out.redlines = reds.length;
  for (let i = from; i < to; i++) {
    STAGE = `ブロック ${i} の概要`;
    const b = blocks[i];
    const item = isTable(b) ? readTable(b, false) : readParagraph(b, false, reds);
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
  const reds = redlineSpans();
  for (let i = a; i <= Math.min(b, a + 100); i++) {
    STAGE = `ブロック ${i} の詳細`;
    const blk = blocks[i];
    const item = isTable(blk) ? readTable(blk, true, reds) : readParagraph(blk, true, reds);
    item.id = blockId(i, blk);
    out.blocks.push(item);
  }
  return out;
};
