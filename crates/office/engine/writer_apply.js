// Shiki Office Engine — Writer への書き込み（#725）。
//
// 受け取る op は Rust（office::engine::writer）が検証・正規化した後の形:
// - 本文の HTML は無害化・CSS のインライン化済み（LibreOffice の HTML 取り込みがそのまま読める形）
// - 段落スタイルの指定は段落の先頭のブックマーク <a name="shiki-s-<16進>-<連番>"> で渡る（取り込み後にここで当てる）
// - 長さは 1/100 mm、色は "#RRGGBB"
// ブロック ID は一括の最初にまとめて解決し、UNO の参照で持つ（途中の挿入・削除で位置がずれても
// 後続の操作の対象が変わらない）。1 回の apply は 1 つの undo 単位で、途中の失敗は丸ごと取り消す。

const PB = I.text.ControlCharacter.PARAGRAPH_BREAK;

/**
 * 取り込む HTML の先頭に Rust が置く番兵の段落の文字列。取り込みの最初の段落は、差し込み先の
 * （分割で作った）段落に流し込まれ、その段落の文字書式（例: 右寄せの日付の 10.5pt）を引き継ぐ。
 * 番兵にそれを吸わせて捨てることで、本物の内容をきれいな段落から始める。
 */
const IMPORT_SENTINEL = "\u2063SHIKI-IMPORT\u2063";

function pv(name, value) {
  return propertyValue(name, value);
}

/** UTF-8 のバイト列（sequence<byte>）を作る。 */
function utf8Bytes(s) {
  const enc = unescape(encodeURIComponent(s));
  const out = new Array(enc.length);
  for (let i = 0; i < enc.length; i++) {
    const b = enc.charCodeAt(i);
    out[i] = b > 127 ? b - 256 : b;
  }
  return out;
}

/** 文書の「本文の既定」スタイル（Word の標準 = LibreOffice の Standard）。 */
function bodyStyle() {
  return "Standard";
}

/** 印（"shiki-s-<16 進の UTF-8>-<連番>"）→ スタイル名。 */
function markerStyleName(marker) {
  const m = /^shiki-s-([0-9a-f]+)-\d+$/.exec(marker);
  if (!m) return null;
  const hex = m[1];
  let enc = "";
  for (let i = 0; i + 1 < hex.length; i += 2) enc += String.fromCharCode(parseInt(hex.slice(i, i + 2), 16));
  try {
    return decodeURIComponent(escape(enc));
  } catch (e) {
    return null;
  }
}

function paragraphStyles() {
  return DOC.getStyleFamilies().getByName("ParagraphStyles");
}

/** スタイルを替えても残す直接書式（LibreOffice は段落スタイルを替えると段落の直接書式を既定へ戻す）。 */
const KEEP_ON_RESTYLE = [
  "ParaAdjust", "ParaLeftMargin", "ParaRightMargin", "ParaFirstLineIndent", "ParaTopMargin",
  "ParaBottomMargin", "ParaLineSpacing", "ParaBackColor", "CharWeight", "CharWeightAsian",
  "CharPosture", "CharPostureAsian", "CharUnderline", "CharStrikeout", "CharHeight", "CharHeightAsian",
  "CharFontName", "CharFontNameAsian", "CharBackColor", "CharColor",
];

function isDirect(obj, name) {
  try {
    return enumName(obj.getPropertyState(name)) === "DIRECT_VALUE";
  } catch (e) {
    return false;
  }
}

/** 段落スタイルを当てる。段落に直接付いた書式（HTML の style・class 由来の色など）は保つ。 */
function setParaStyle(p, name) {
  const kept = [];
  for (const n of KEEP_ON_RESTYLE) {
    if (isDirect(p, n)) kept.push([n, p.getPropertyValue(n)]);
  }
  p.setPropertyValue("ParaStyleName", name);
  for (const [n, v] of kept) {
    try {
      if (n === "CharColor") setCharColorW(p, v);
      else p.setPropertyValue(n, v);
    } catch (e) {
      // 戻せない値は既定のまま。
    }
  }
}

/** 段落の中の data-style の印（Rust が先頭に置いたブックマーク）の名前。 */
function styleMarkerOf(p) {
  try {
    const en = p.createEnumeration();
    while (en.hasMoreElements()) {
      const r = en.nextElement();
      if (r.getPropertyValue("TextPortionType") !== "Bookmark") continue;
      const n = r.getPropertyValue("Bookmark").getName();
      if (n.startsWith("shiki-s-")) return n;
    }
  } catch (e) {
    // 列挙できない段落。
  }
  return null;
}

/**
 * HTML の取り込みが付けたスタイルを整える。
 * - "Text body"（HTML の <p>）や class 由来の派生スタイルは、文書の本文スタイルへ
 * - 表のセルの "Table Contents" / "Table Heading" も本文スタイルへ（docx に保存すると独自スタイルとして
 *   残るため）。見出しセルの太字・中央揃えと、セル内の詰めた間隔は直接書式で付ける
 * - data-style の指定は、Rust が段落の先頭に置いたブックマーク（<a name="shiki-s-…">）で運ばれる。
 *   既存の文書へ差し込むとき取り込みは class から派生スタイルを作らないので、印にはブックマークを使う
 */
function fixImportedStyles(paragraphs) {
  const styles = paragraphStyles();
  const missing = [];
  for (const p of paragraphs) {
    let name;
    try {
      name = p.getPropertyValue("ParaStyleName");
    } catch (e) {
      continue;
    }
    const marker = styleMarkerOf(p);
    const target = marker ? markerStyleName(marker) : null;
    if (target) {
      if (styles.hasByName(target)) setParaStyle(p, target);
      else missing.push(target);
      continue;
    }
    if (name === "Table Heading" || name === "Table Contents") {
      const heading = name === "Table Heading";
      const center = heading && !isDirect(p, "ParaAdjust");
      const bold = heading && !isDirect(p, "CharWeight");
      const top = !isDirect(p, "ParaTopMargin");
      const bottom = !isDirect(p, "ParaBottomMargin");
      const line = !isDirect(p, "ParaLineSpacing");
      setParaStyle(p, bodyStyle());
      if (center) p.setPropertyValue("ParaAdjust", I.style.ParagraphAdjust.CENTER);
      if (bold) {
        p.setPropertyValue("CharWeight", 150);
        p.setPropertyValue("CharWeightAsian", 150);
      }
      if (top) p.setPropertyValue("ParaTopMargin", long(0));
      if (bottom) p.setPropertyValue("ParaBottomMargin", long(0));
      if (line) {
        const ls = new I.style.LineSpacing();
        ls.Mode = 0;
        ls.Height = 100;
        p.setPropertyValue("ParaLineSpacing", ls);
      }
    } else if (name === "Text body" || name === "Body Text" || name.indexOf(".") > 0) {
      setParaStyle(p, bodyStyle());
    }
  }
  // 印のブックマークは役目を終えたので消す（対象外の段落に残ったものも含めて）。
  const bookmarks = DOC.getBookmarks();
  for (const n of Array.from(bookmarks.getElementNames())) {
    if (n.startsWith("shiki-s-")) bookmarks.getByName(n).dispose();
  }
  if (missing.length) fail("style_not_found", `段落スタイル「${missing[0]}」がありません（office.read の概要に使えるスタイルがあります）`);
}

/** 段落・表のセルの中の段落も含めて、指定したブロックの段落を集める。 */
function collectParagraphs(blocks) {
  const out = [];
  for (const b of blocks) {
    if (isTable(b)) {
      for (const n of Array.from(b.getCellNames())) {
        const en = b.getCellByName(n).getText().createEnumeration();
        while (en.hasMoreElements()) {
          const p = en.nextElement();
          if (!isTable(p)) out.push(p);
        }
      }
    } else {
      out.push(b);
    }
  }
  return out;
}

function sameObject(a, b) {
  try {
    return uno.sameUnoObject(a, b);
  } catch (e) {
    return a === b;
  }
}

function indexOfBlock(blocks, obj) {
  for (let i = 0; i < blocks.length; i++) if (sameObject(blocks[i], obj)) return i;
  return -1;
}

/**
 * HTML を、anchor（ブロック）の後ろ（before=true なら前）へ差し込む。差し込んだブロックを返す。
 * anchor が null なら文書の末尾。
 */
/**
 * anchor の後ろ（before=true なら前）に空の段落を作って返す。anchor が null なら文書の末尾。
 *
 * 新しい段落オブジェクトを段落の後ろへ insertTextContentAfter で入れることは拒否される（表の前後は通る・
 * 実機で確認）ので、段落の後ろへは「段落の末尾で段落を分割する」ことで空の段落を作る。
 */
function emptyParagraphAt(anchor, before) {
  const text = DOC.getText();
  const all = bodyBlocks();
  if (anchor === null) {
    return emptyParagraphAt(all[all.length - 1], false);
  }
  if (isTable(anchor)) {
    const para = DOC.createInstance("com.sun.star.text.Paragraph");
    if (before) text.insertTextContentBefore(para, anchor);
    else text.insertTextContentAfter(para, anchor);
    return para;
  }
  const index = indexOfBlock(all, anchor);
  if (before) {
    if (index > 0) return emptyParagraphAt(all[index - 1], false);
    // 文書の先頭: 先頭の段落の頭で分割すると、前側（空）が新しい先頭の段落になる。
    const cur = text.createTextCursorByRange(anchor.getStart());
    text.insertControlCharacter(cur, PB, false);
    return resetParagraph(bodyBlocks()[0]);
  }
  const cur = text.createTextCursorByRange(anchor.getEnd());
  text.insertControlCharacter(cur, PB, false);
  return resetParagraph(bodyBlocks()[index + 1]);
}

/** 段落を分割すると、元の段落の直接書式（右寄せ・字下げなど）を引き継ぐ。既定へ戻す。 */
const RESET_PARA_PROPS = [
  "ParaAdjust", "ParaLeftMargin", "ParaRightMargin", "ParaFirstLineIndent", "ParaTopMargin",
  "ParaBottomMargin", "ParaLineSpacing", "ParaBackColor", "BreakType", "NumberingStyleName",
  "CharWeight", "CharWeightAsian", "CharPosture", "CharColor", "CharHeight", "CharHeightAsian",
  "CharUnderline", "CharFontName", "CharFontNameAsian", "CharBackColor",
];

function resetParagraph(p) {
  p.setPropertyValue("ParaStyleName", bodyStyle());
  for (const name of RESET_PARA_PROPS) {
    try {
      p.setPropertyToDefault(name);
    } catch (e) {
      // 既定に戻せないプロパティは残す。
    }
  }
  return p;
}

/**
 * HTML を、anchor（ブロック）の後ろ（before=true なら前）へ差し込む。差し込んだブロックを返す。
 * anchor が null なら文書の末尾。
 */
function insertHtml(anchor, before, html, created) {
  const text = DOC.getText();
  const para = emptyParagraphAt(anchor, before);
  // 差し込み前後の境界（直前のブロックと直後のブロック）を覚える。
  let blocks = bodyBlocks();
  const pos = indexOfBlock(blocks, para);
  const prev = pos > 0 ? blocks[pos - 1] : null;
  const next = pos + 1 < blocks.length ? blocks[pos + 1] : null;
  STAGE = "HTML の取り込み";
  const stream = I.io.SequenceInputStream.createStreamFromSequence(uno.componentContext, utf8Bytes(html));
  const cur = text.createTextCursorByRange(para.getStart());
  cur.insertDocumentFromURL("private:stream", [pv("InputStream", stream), pv("FilterName", "HTML (StarWriter)")]);
  STAGE = "取り込み後の整え";
  blocks = bodyBlocks();
  const start = prev === null ? 0 : indexOfBlock(blocks, prev) + 1;
  const end = next === null ? blocks.length : indexOfBlock(blocks, next);
  let inserted = blocks.slice(start, end);
  // 先頭は Rust が置いた番兵の段落（分割した段落の書式を吸わせるためのもの）。捨てる。
  if (inserted.length > 1 && !isTable(inserted[0]) && inserted[0].getString() === IMPORT_SENTINEL) {
    inserted[0].dispose();
    inserted = inserted.slice(1);
  }
  // 取り込みは末尾に空段落を残す（差し込み先の段落の残り）。空の段落を後ろから消す。
  while (inserted.length > 1) {
    const last = inserted[inserted.length - 1];
    if (isTable(last) || last.getString() !== "") break;
    last.dispose();
    inserted = inserted.slice(0, -1);
  }
  fixImportedStyles(collectParagraphs(inserted));
  return inserted;
}

/** 取り込みが作った派生スタイルのうち、使われなくなったものを消す。 */
function removeCreatedStyles(created) {
  const styles = paragraphStyles();
  for (const name of created) {
    try {
      if (styles.hasByName(name) && !styles.getByName(name).isInUse()) styles.removeByName(name);
    } catch (e) {
      // 既定のスタイルは消せない（残っても害はない）。
    }
  }
}

const ADJUST = { left: "LEFT", center: "CENTER", right: "RIGHT", justify: "BLOCK" };

function setCharColorW(target, color) {
  if (color !== -1) {
    try {
      const unused = target.getPropertyValue("CharBackgroundComplexColor");
      if (unused && unused.getType && unused.getType() === 0) target.setPropertyValue("CharComplexColor", unused);
    } catch (e) {
      // 複合色を持たない版。
    }
  }
  target.setPropertyValue("CharColor", long(color));
}

function applyFont(target, f) {
  if (!f) return;
  if (f.name !== undefined) {
    target.setPropertyValue("CharFontName", f.name);
    target.setPropertyValue("CharFontNameAsian", f.name);
  }
  if (f.size !== undefined) {
    target.setPropertyValue("CharHeight", f.size);
    target.setPropertyValue("CharHeightAsian", f.size);
  }
  if (f.bold !== undefined) {
    const w = f.bold ? 150 : 100;
    target.setPropertyValue("CharWeight", w);
    target.setPropertyValue("CharWeightAsian", w);
  }
  if (f.italic !== undefined) {
    const p = f.italic ? I.awt.FontSlant.ITALIC : I.awt.FontSlant.NONE;
    target.setPropertyValue("CharPosture", p);
    target.setPropertyValue("CharPostureAsian", p);
  }
  if (f.underline !== undefined) target.setPropertyValue("CharUnderline", short(f.underline ? 1 : 0));
  if (f.strike !== undefined) target.setPropertyValue("CharStrikeout", short(f.strike ? 1 : 0));
  if (f.color !== undefined) setCharColorW(target, hexToColor(f.color));
  if (f.highlight !== undefined) target.setPropertyValue("CharBackColor", long(hexToColor(f.highlight)));
}

function applyParagraph(p, op) {
  if (op.style !== undefined) {
    if (!paragraphStyles().hasByName(op.style)) fail("style_not_found", `段落スタイル「${op.style}」がありません`);
    setParaStyle(p, op.style);
  }
  if (op.align !== undefined) p.setPropertyValue("ParaAdjust", I.style.ParagraphAdjust[ADJUST[op.align]]);
  if (op.indent_left !== undefined) p.setPropertyValue("ParaLeftMargin", long(op.indent_left));
  if (op.indent_first !== undefined) p.setPropertyValue("ParaFirstLineIndent", long(op.indent_first));
  if (op.space_before !== undefined) p.setPropertyValue("ParaTopMargin", long(op.space_before));
  if (op.space_after !== undefined) p.setPropertyValue("ParaBottomMargin", long(op.space_after));
  if (op.line_spacing !== undefined) {
    const ls = new I.style.LineSpacing();
    ls.Mode = op.line_spacing.mode;
    ls.Height = op.line_spacing.height;
    p.setPropertyValue("ParaLineSpacing", ls);
  }
  if (op.keep_with_next !== undefined) p.setPropertyValue("ParaKeepTogether", op.keep_with_next);
  if (op.page_break_before !== undefined) {
    p.setPropertyValue("BreakType", op.page_break_before ? I.style.BreakType.PAGE_BEFORE : I.style.BreakType.NONE);
  }
  applyFont(p, op.font);
}

/** from〜to のブロック（両端を含む）。 */
function blockRange(resolved, from, to) {
  const all = bodyBlocks();
  const a = indexOfBlock(all, resolved[from]);
  const b = to ? indexOfBlock(all, resolved[to]) : a;
  if (a < 0 || b < 0) fail("block_gone", "対象のブロックはこの一括の中で既に消えています");
  if (b < a) fail("bad_range", "範囲の終わりが始まりより前です");
  return all.slice(a, b + 1);
}

/** 検索の範囲（ブロックの範囲・無ければ文書全体）で、文字列を探して見つかった範囲を返す。 */
function findAll(op, resolved) {
  const sd = DOC.createSearchDescriptor();
  sd.setSearchString(op.find);
  sd.setPropertyValue("SearchCaseSensitive", !!op.match_case);
  sd.setPropertyValue("SearchWords", !!op.whole_word);
  const found = DOC.findAll(sd);
  const hits = [];
  let scope = null;
  if (op.from) scope = collectParagraphs(blockRange(resolved, op.from, op.to));
  for (let i = 0; i < found.getCount(); i++) {
    const r = found.getByIndex(i);
    if (scope) {
      const inScope = scope.some((p) => {
        try {
          const cmp = DOC.getText().compareRegionStarts(p, r) >= 0 && DOC.getText().compareRegionEnds(r, p) >= 0;
          return cmp;
        } catch (e) {
          return false;
        }
      });
      if (!inScope) continue;
    }
    hits.push(r);
  }
  return hits;
}

const WOPS = {};

/** 白紙の文書（空の段落 1 つ）なら、その段落を返す。差し込んだ後に消すため。 */
function blankParagraph() {
  const all = bodyBlocks();
  return all.length === 1 && !isTable(all[0]) && all[0].getString() === "" ? all[0] : null;
}

/**
 * 差し込みの後に残った空の段落を消す（白紙の文書の段落・目次の置き場所に作った段落）。
 * 表の直後の最後の段落は Word でも必須なので残す。
 */
function dropEmptyParagraph(p) {
  if (p === null || p.getString() !== "") return;
  const all = bodyBlocks();
  const i = indexOfBlock(all, p);
  if (i < 0 || all.length < 2) return;
  if (i === all.length - 1 && isTable(all[i - 1])) return;
  // 先頭の段落はページスタイル（用紙・余白）を持っていることがある。次のブロックへ引き継ぐ。
  try {
    const page = p.getPropertyValue("PageDescName");
    if (i === 0 && page) all[1].setPropertyValue("PageDescName", page);
  } catch (e) {
    // ページスタイルを持たない段落。
  }
  p.dispose();
}

WOPS.insert_html = (op, resolved, created) => {
  const blank = blankParagraph();
  let anchor = op.anchor === null ? null : resolved[op.anchor];
  let before = op.before;
  if (op.at === "start") {
    anchor = bodyBlocks()[0];
    before = true;
  }
  const inserted = insertHtml(anchor, before, op.html, created);
  dropEmptyParagraph(blank);
  return { blocks: inserted.length };
};

WOPS.replace_html = (op, resolved, created) => {
  const range = blockRange(resolved, op.from, op.to);
  const inserted = insertHtml(range[range.length - 1], false, op.html, created);
  STAGE = "置き換え前のブロックの削除";
  for (const b of range) b.dispose();
  return { removed: range.length, blocks: inserted.length };
};

WOPS.delete = (op, resolved) => {
  const range = blockRange(resolved, op.from, op.to);
  if (range.length >= bodyBlocks().length) fail("delete_all", "文書のすべてのブロックは消せません（最低 1 段落は残ります）");
  for (const b of range) b.dispose();
  return { removed: range.length };
};

WOPS.replace_text = (op, resolved) => {
  const hits = findAll(op, resolved);
  if (hits.length === 0) fail("not_found", `「${op.find}」が見つかりません`);
  if (!op.all && hits.length > 1) {
    fail("ambiguous", `「${op.find}」が ${hits.length} か所あります。範囲（from/to）で絞るか all: true にしてください`);
  }
  for (const r of hits) r.setString(op.replace);
  return { replaced: hits.length };
};

WOPS.format_text = (op, resolved) => {
  const hits = findAll(op, resolved);
  if (hits.length === 0) fail("not_found", `「${op.find}」が見つかりません`);
  if (!op.all && hits.length > 1) {
    fail("ambiguous", `「${op.find}」が ${hits.length} か所あります。範囲（from/to）で絞るか all: true にしてください`);
  }
  for (const r of hits) applyFont(r, op.font);
  return { formatted: hits.length };
};

WOPS.format_paragraph = (op, resolved) => {
  const range = blockRange(resolved, op.from, op.to);
  const paras = op.include_tables ? collectParagraphs(range) : range.filter((b) => !isTable(b));
  for (const p of paras) applyParagraph(p, op);
  return { paragraphs: paras.length };
};

WOPS.set_style = (op) => {
  const fam = DOC.getStyleFamilies().getByName(op.family === "character" ? "CharacterStyles" : "ParagraphStyles");
  let st;
  if (fam.hasByName(op.name)) {
    st = fam.getByName(op.name);
  } else {
    st = DOC.createInstance(op.family === "character" ? "com.sun.star.style.CharacterStyle" : "com.sun.star.style.ParagraphStyle");
    fam.insertByName(op.name, st);
  }
  if (op.based_on) {
    if (!fam.hasByName(op.based_on)) fail("style_not_found", `基にするスタイル「${op.based_on}」がありません`);
    st.setParentStyle(op.based_on);
  }
  if (op.family === "character") {
    applyFont(st, op.font);
  } else {
    applyParagraph(st, Object.assign({}, op, { style: undefined }));
  }
  return {};
};

function pageStyle() {
  const first = bodyBlocks()[0];
  let name = "Standard";
  try {
    name = first.getPropertyValue("PageDescName") || "Standard";
  } catch (e) {
    name = "Standard";
  }
  return DOC.getStyleFamilies().getByName("PageStyles").getByName(name);
}

WOPS.page_setup = (op) => {
  const st = pageStyle();
  if (op.paper) {
    st.setPropertyValue("Width", long(op.paper[0]));
    st.setPropertyValue("Height", long(op.paper[1]));
  }
  if (op.landscape !== undefined) {
    const w = st.getPropertyValue("Width");
    const h = st.getPropertyValue("Height");
    st.setPropertyValue("IsLandscape", op.landscape);
    if (op.landscape !== w > h) {
      st.setPropertyValue("Width", long(h));
      st.setPropertyValue("Height", long(w));
    }
  }
  if (op.margins) {
    const [t, r, b, l] = op.margins;
    st.setPropertyValue("TopMargin", long(t));
    st.setPropertyValue("RightMargin", long(r));
    st.setPropertyValue("BottomMargin", long(b));
    st.setPropertyValue("LeftMargin", long(l));
  }
  return {};
};

/** ヘッダー・フッター。pieces は文字とフィールド（ページ番号など）の並び。null で消す。 */
WOPS.header_footer = (op) => {
  const st = pageStyle();
  const key = op.which === "header" ? "Header" : "Footer";
  if (op.content === null) {
    st.setPropertyValue(key + "IsOn", false);
    return {};
  }
  st.setPropertyValue(key + "IsOn", true);
  const text = st.getPropertyValue(key + "Text");
  text.setString("");
  const cur = text.createTextCursor();
  const kinds = { page: "PageNumber", pages: "PageCount", date: "DateTime", title: "DocInfo.Title", file: "FileName" };
  for (const piece of op.content.pieces) {
    if (piece.t === "text") {
      text.insertString(cur, piece.v, false);
    } else {
      const field = DOC.createInstance("com.sun.star.text.TextField." + kinds[piece.t]);
      if (piece.t === "page" || piece.t === "pages") {
        // 算用数字（com.sun.star.style.NumberingType.ARABIC = 4）。既定は文字（A, B, …）になることがある。
        field.setPropertyValue("NumberingType", short(4));
      }
      if (piece.t === "page") field.setPropertyValue("SubType", I.text.PageNumberType.CURRENT);
      if (piece.t === "date") field.setPropertyValue("IsDate", true);
      text.insertTextContent(cur, field, false);
    }
  }
  cur.gotoStart(false);
  cur.gotoEnd(true);
  cur.setPropertyValue("ParaAdjust", I.style.ParagraphAdjust[ADJUST[op.content.align || "center"]]);
  if (op.content.size) cur.setPropertyValue("CharHeight", op.content.size);
  return {};
};

WOPS.insert_toc = (op, resolved) => {
  const text = DOC.getText();
  // 目次は文書の先頭（anchor が null）か、指定したブロックの前後に置く。
  let para;
  if (op.anchor !== null) para = emptyParagraphAt(resolved[op.anchor], op.before);
  else if (op.at === "end") para = emptyParagraphAt(null, false);
  else para = emptyParagraphAt(bodyBlocks()[0], true);
  const toc = DOC.createInstance("com.sun.star.text.ContentIndex");
  const cur = text.createTextCursorByRange(para.getStart());
  text.insertTextContent(cur, toc, false);
  toc.setPropertyValue("Title", op.title);
  toc.setPropertyValue("CreateFromOutline", true);
  toc.setPropertyValue("Level", short(op.levels));
  toc.update();
  dropEmptyParagraph(para);
  return {};
};

WOPS.update_indexes = () => {
  const idx = DOC.getDocumentIndexes();
  for (let i = 0; i < idx.getCount(); i++) idx.getByIndex(i).update();
  return { updated: idx.getCount() };
};

WOPS.page_break = (op, resolved) => {
  const range = blockRange(resolved, op.from, null);
  range[0].setPropertyValue("BreakType", I.style.BreakType.PAGE_BEFORE);
  return {};
};

WOPS.track_changes = (op) => {
  DOC.setPropertyValue("RecordChanges", op.enabled);
  return {};
};

HANDLERS["writer.apply"] = (req) => {
  requireKind("writer");
  const results = [];
  const created = new Set();
  atomically(req.label, () => {
    // ブロック ID を先にまとめて解決する（以降は参照で扱う）。
    STAGE = "ブロック ID の解決";
    const blocks = bodyBlocks();
    const resolved = {};
    for (const id of req.block_ids) resolved[id] = blocks[resolveBlock(blocks, id)];
    for (let i = 0; i < req.ops.length; i++) {
      const op = req.ops[i];
      const fn = WOPS[op.op];
      STAGE = `${i + 1} 件目（${op.op}）`;
      if (!fn) fail("unknown_op", `未知の操作です: ${op.op}`, { op_index: i });
      try {
        results.push(Object.assign({ op: op.op }, fn(op, resolved, created)));
      } catch (e) {
        const d = describeError(e);
        d.op_index = i;
        throw new EngineError(d.code, d.message, d);
      }
    }
    removeCreatedStyles(created);
  });
  return { results, count: bodyBlocks().length };
};
