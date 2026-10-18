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

/** 段落の中の印（Rust が先頭に置いたブックマーク "shiki-…"）の名前。 */
function markersOf(p) {
  const out = [];
  try {
    const en = p.createEnumeration();
    while (en.hasMoreElements()) {
      const r = en.nextElement();
      if (r.getPropertyValue("TextPortionType") !== "Bookmark") continue;
      const n = r.getPropertyValue("Bookmark").getName();
      if (n.startsWith("shiki-")) out.push(n);
    }
  } catch (e) {
    // 列挙できない段落。
  }
  return out;
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
    const markers = markersOf(p);
    // HTML で指定の無い段落の間隔（取り込みが付ける上 0・下 10pt 相当）はスタイルの値へ戻す。
    // 上と下は 1 つの属性なので、片方だけ既定へ戻すことはできない。両方を戻し、スタイルを当てた後で
    // 指定した側だけを付け直す（先に付け直すと、もう片方に元のスタイルの値が直接書式として残る）。
    const kept = (markers.find((m) => m.startsWith("shiki-k-")) || "").split("-")[2] || "";
    let spacing = null;
    try {
      spacing = [p.getPropertyValue("ParaTopMargin"), p.getPropertyValue("ParaBottomMargin")];
      p.setPropertyToDefault("ParaTopMargin");
    } catch (e) {
      // 既定へ戻せない段落。
    }
    restyleImported(p, name, markers, styles, missing);
    if (spacing !== null) {
      if (kept.indexOf("t") >= 0) p.setPropertyValue("ParaTopMargin", long(spacing[0]));
      if (kept.indexOf("b") >= 0) p.setPropertyValue("ParaBottomMargin", long(spacing[1]));
    }
    // page-break-after: avoid（次の段落と同じページに置く）。
    if (kept.indexOf("n") >= 0) p.setPropertyValue("ParaKeepTogether", true);
    // 空の段落（<p></p>）は取り込みが捨てるので、Rust が空白を入れて印を付けている。空に戻す。
    if (kept.indexOf("e") >= 0) p.setString("");
    // 書体・大きさを HTML で指定していない段落は、取り込みが付けた既定の書体・大きさを消してスタイルに従わせる。
    if (kept.indexOf("f") < 0) clearImportedFonts(p);
    else unifyScripts(p);
  }
  tightenImportedLists(paragraphs);
  // 印のブックマークは役目を終えたので消す（対象外の段落に残ったものも含めて）。
  const bookmarks = DOC.getBookmarks();
  for (const n of Array.from(bookmarks.getElementNames())) {
    if (n.startsWith("shiki-s-") || n.startsWith("shiki-k-")) bookmarks.getByName(n).dispose();
  }
  if (missing.length) fail("style_not_found", `段落スタイル「${missing[0]}」がありません（office.read の概要に使えるスタイルがあります）`);
}

const IMPORTED_FONT_PROPS = [
  "CharFontName", "CharFontNameAsian", "CharFontNameComplex", "CharHeight", "CharHeightAsian", "CharHeightComplex",
];

/** 段落と段落内の文字から、書体と大きさの直接書式を消す（表のセルの段落はセルの文字列で範囲を作る）。 */
function clearImportedFonts(p) {
  let cur = null;
  try {
    cur = p.getText().createTextCursorByRange(p.getStart());
    cur.gotoRange(p.getEnd(), true);
  } catch (e) {
    cur = null;
  }
  for (const n of IMPORTED_FONT_PROPS) {
    try {
      if (cur !== null) cur.setPropertyToDefault(n);
      p.setPropertyToDefault(n);
    } catch (e) {
      // 戻せないプロパティ。
    }
  }
}

/** 箇条書きの 1 段の字下げ（Word の既定と同じ 0.74cm＝10.5pt の 2 文字分）。 */
const LIST_STEP = 740;

/**
 * 取り込みが作った箇条書きの番号スタイル（名前が数字だけ）の字下げを Word の既定に揃える
 * （取り込みは 1 段 12.5mm と広い）。記号は (段)×7.4mm、本文は (段+1)×7.4mm。
 */
function tightenImportedLists(paragraphs) {
  const fam = DOC.getStyleFamilies().getByName("NumberingStyles");
  const done = new Set();
  for (const p of paragraphs) {
    let name = "";
    try {
      name = p.getPropertyValue("NumberingStyleName");
    } catch (e) {
      continue;
    }
    if (!/^\d+$/.test(name) || done.has(name) || !fam.hasByName(name)) continue;
    done.add(name);
    try {
      const style = fam.getByName(name);
      const rules = style.getPropertyValue("NumberingRules");
      for (let i = 0; i < rules.getCount(); i++) {
        const level = Array.from(rules.getByIndex(i)).map((v) => {
          if (v.Name === "LeftMargin") return pv("LeftMargin", long((i + 1) * LIST_STEP));
          if (v.Name === "FirstLineOffset") return pv("FirstLineOffset", long(-LIST_STEP));
          return v;
        });
        rules.replaceByIndex(i, level);
      }
      style.setPropertyValue("NumberingRules", rules);
    } catch (e) {
      // 字下げを変えられない番号スタイル（取り込みの既定のまま残す）。
    }
  }
}

/** 文字種（欧文・和文・CTL）ごとに分かれた文字の書式。 */
const SCRIPT_PROPS = [
  ["CharHeight", "CharHeightAsian", "CharHeightComplex"],
  ["CharWeight", "CharWeightAsian", "CharWeightComplex"],
  ["CharPosture", "CharPostureAsian", "CharPostureComplex"],
  ["CharFontName", "CharFontNameAsian"],
];

/**
 * 取り込みは CSS の大きさ・太さ・書体を、文字列の文字種に当たるプロパティにだけ付ける（和文には
 * CharHeightAsian だけ）。行頭の「※」のような文字種の決まらない文字は欧文の値で描かれるので、
 * 指定が一部の文字種にしか無いときは全部に写す（CSS どおり、文字種によらず効かせる）。
 */
function unifyScripts(p) {
  const runs = [];
  try {
    const en = p.createEnumeration();
    while (en.hasMoreElements()) {
      const r = en.nextElement();
      if (r.getPropertyValue("TextPortionType") === "Text") runs.push(r);
    }
  } catch (e) {
    return;
  }
  for (const r of runs) {
    for (const group of SCRIPT_PROPS) {
      const direct = group.filter((n) => isDirect(r, n));
      if (direct.length === 0 || direct.length === group.length) continue;
      const v = r.getPropertyValue(direct.indexOf(group[1]) >= 0 ? group[1] : direct[0]);
      for (const n of group) if (direct.indexOf(n) < 0) r.setPropertyValue(n, v);
    }
  }
  // 記号だけの連（行頭の「※」「●」「【」など）は取り込みが書式の範囲から外すことがある。隣の連に揃える。
  for (let i = 0; i < runs.length; i++) {
    if (/[\p{L}\p{N}]/u.test(runs[i].getString())) continue;
    const near = [runs[i + 1], runs[i - 1]].filter((x) => x !== undefined);
    for (const group of SCRIPT_PROPS) {
      if (group.some((n) => isDirect(runs[i], n))) continue;
      const from = near.find((x) => group.every((n) => isDirect(x, n)));
      if (!from) continue;
      for (const n of group) runs[i].setPropertyValue(n, from.getPropertyValue(n));
    }
  }
}

/** 取り込んだ段落 1 つのスタイルを整える（data-style の印・表のセル・本文）。 */
function restyleImported(p, name, markers, styles, missing) {
  const marker = markers.find((m) => m.startsWith("shiki-s-"));
  const target = marker ? markerStyleName(marker) : null;
  if (target) {
    if (styles.hasByName(target)) setParaStyle(p, target);
    else missing.push(target);
    return;
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
      p.setPropertyValue("CharWeight", float(150));
      p.setPropertyValue("CharWeightAsian", float(150));
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
  if (anchor === null) return emptyParagraphAt(all[all.length - 1], false);
  const index = indexOfBlock(all, anchor);
  if (index < 0) fail("block_gone", GONE_MESSAGE);
  if (isTable(anchor)) {
    insertParagraphBeside(anchor, before);
    return resetParagraph(bodyBlocks()[before ? index : index + 1]);
  }
  // 目次などの索引（セクション）の中へは入れない。前後に段落を置く。
  const section = indexSectionOf(anchor);
  if (section !== null) {
    const span = indexSpan(all, anchor);
    if (before || span[1] + 1 >= all.length) {
      insertParagraphBeside(section, before);
      return resetParagraph(bodyBlocks()[before ? span[0] : span[1] + 1]);
    }
    return emptyParagraphAt(all[span[1] + 1], true);
  }
  // 後ろに次のブロックがあれば、その前に作る（段落の頭で分割する）。
  if (!before && index + 1 < all.length) return emptyParagraphAt(all[index + 1], true);
  if (before) {
    // 段落の頭で分割すると前側（空）が新しい段落になり、anchor の参照は内容に付いたまま。
    // 改ページは内容の側に残す（分割で前側へ移るため）。
    const brk = isDirect(anchor, "BreakType") ? anchor.getPropertyValue("BreakType") : null;
    const cur = text.createTextCursorByRange(anchor.getStart());
    text.insertControlCharacter(cur, PB, false);
    const para = resetParagraph(bodyBlocks()[index]);
    if (brk !== null) anchor.setPropertyValue("BreakType", brk);
    return para;
  }
  // 文書の最後の段落の後ろ: 末尾で分割するしかない。Writer は元の段落の参照を後ろ側（新しい空の段落）へ
  // 移すので、この一括で解決済みの参照を内容の側へ付け替える。
  const cur = text.createTextCursorByRange(anchor.getEnd());
  text.insertControlCharacter(cur, PB, false);
  const blocks = bodyBlocks();
  rebindResolved(anchor, blocks[index]);
  return resetParagraph(blocks[index + 1]);
}

/**
 * 表・セクションの前後に空の段落を入れる。
 *
 * 作った段落オブジェクトは文書に入った後も壊れていて（"SwXParagraph: disposed or invalid"）、JS 側で
 * 参照を持っている間は列挙しても同じオブジェクトが返る。関数の中で作って参照をすぐ手放し、
 * 位置で取り直す（実機で確認）。
 */
function insertParagraphBeside(content, before) {
  const text = DOC.getText();
  const para = DOC.createInstance("com.sun.star.text.Paragraph");
  if (before) text.insertTextContentBefore(para, content);
  else text.insertTextContentAfter(para, content);
}

/** 段落が目次などの索引の中にあれば、その索引（無ければ null）。 */
function documentIndexOf(b) {
  if (isTable(b)) return null;
  try {
    return b.getPropertyValue("DocumentIndex") || null;
  } catch (e) {
    return null;
  }
}

/**
 * 索引の中の段落なら、その索引を包むいちばん外側のセクション（無ければ null）。
 * docx を開き直した目次は、索引のセクションの内側にさらにセクションを持つ。内側の前に入れると
 * 目次の中に入ってしまうので、外側まで辿る。
 */
function indexSectionOf(b) {
  if (documentIndexOf(b) === null) return null;
  try {
    let section = b.getPropertyValue("TextSection");
    while (section && section.getParentSection()) section = section.getParentSection();
    return section || null;
  } catch (e) {
    return null;
  }
}

/** b を含む索引が占めるブロックの範囲 [最初, 最後]。 */
function indexSpan(all, b) {
  const idx = documentIndexOf(b);
  const i = indexOfBlock(all, b);
  let first = i;
  let last = i;
  while (first > 0 && sameObject(documentIndexOf(all[first - 1]), idx)) first--;
  while (last + 1 < all.length && sameObject(documentIndexOf(all[last + 1]), idx)) last++;
  return [first, last];
}

const GONE_MESSAGE =
  "対象のブロックはこの一括の前の操作（replace / delete）で既に消えています。置き換えた内容を基準にする操作や、" +
  "置き換えた内容への書式付けは次の呼び出しに分け、結果に返るブロック ID を使ってください。文書の末尾に足すなら at: \"end\"" +
  "（段落の間隔や「次の段落と同じページ」は html の style でも指定できます）";

/** この一括で解決済みのブロック参照（ID → UNO の参照）。writer.apply が入れる。 */
let RESOLVED = {};

/**
 * この一括の replace で置き換えたブロックの ID → 置き換え後のブロックの並び。
 *
 * 「X を置き換えて、X の後ろに足す・X の表を整える」を同じ一括に書くと、X はもう無い。モデルが
 * 自然に書く形（Gemma 4 の評価で繰り返し起きた）なので、X は置き換え後の中身を指すことにする:
 * 後ろへの差し込みは最後のブロックの後ろ、前への差し込みは最初のブロックの前、表の操作は
 * 中身の表（1 つのとき）、範囲は最初から最後まで。
 */
let REPLACED = {};

/** この一括で最後に差し込んだ表（table: "last" で指す）。 */
let LAST_TABLE = null;

/** ID の参照。role は "after" / "before" / "from" / "to" / "table"。 */
function refOf(resolved, id, role) {
  if (!REPLACED[id]) return resolved[id];
  // 後片付け（表の前後の区切り）で消えたブロックは除く。
  const all = bodyBlocks();
  const fresh = REPLACED[id].filter((b) => indexOfBlock(all, b) >= 0);
  if (fresh.length === 0) return resolved[id];
  if (role === "table") {
    const tables = fresh.filter(isTable);
    return tables.length === 1 ? tables[0] : resolved[id];
  }
  return role === "before" || role === "from" ? fresh[0] : fresh[fresh.length - 1];
}

function rebindResolved(stale, fresh) {
  for (const k of Object.keys(RESOLVED)) {
    if (sameObject(RESOLVED[k], stale)) RESOLVED[k] = fresh;
  }
}

/** 変更履歴を記録中か。 */
function recording() {
  try {
    return !!DOC.getPropertyValue("RecordChanges");
  } catch (e) {
    return false;
  }
}

/** 変更履歴の記録を止めて fn を実行する（差し込みの後片付けなど、履歴に残すべきでない操作）。 */
function withoutRecording(fn) {
  const was = recording();
  if (was) DOC.setPropertyValue("RecordChanges", false);
  try {
    return fn();
  } finally {
    if (was) DOC.setPropertyValue("RecordChanges", true);
  }
}

/**
 * ブロックを消す。変更履歴の記録中は段落の削除として履歴に残す（dispose は記録中は何もしない）。
 * 段落の区切りも含めて消し、前後の段落とつなげる。
 */
function removeBlock(b) {
  // 目次の段落は、目次ごと（見出しも含めて）消す。
  const idx = documentIndexOf(b);
  if (idx !== null) {
    // 索引を dispose するだけでは中の段落（見出し・項目）が普通の段落として残る。中身を空にしてから消す。
    const cur = DOC.getText().createTextCursorByRange(idx.getAnchor());
    cur.setString("");
    idx.dispose();
    return;
  }
  if (isTable(b) || !recording()) {
    b.dispose();
    return;
  }
  const text = DOC.getText();
  const all = bodyBlocks();
  const i = indexOfBlock(all, b);
  const prev = i > 0 ? all[i - 1] : null;
  const next = i + 1 < all.length ? all[i + 1] : null;
  // 段落の頭から次の段落の頭までを消す（この段落の区切りだけを消し、前後の段落の区切りには触れない。
  // 直前の区切りから消すと、記録中は直前の段落の書式がこの段落へ写る）。replace は新しい内容を
  // 古いブロックの前に入れるので、次の段落は新しい内容ではない。
  let cur;
  if (next && !isTable(next)) {
    cur = text.createTextCursorByRange(b.getStart());
    cur.gotoRange(next.getStart(), true);
  } else if (prev && !isTable(prev)) {
    cur = text.createTextCursorByRange(prev.getEnd());
    cur.gotoRange(b.getEnd(), true);
  } else {
    cur = text.createTextCursorByRange(b.getStart());
    cur.gotoRange(b.getEnd(), true);
  }
  cur.setString("");
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
  const inserted = insertHtmlBlocks(anchor, before, html, created);
  const tables = inserted.filter(isTable);
  if (tables.length > 0) LAST_TABLE = tables[tables.length - 1];
  return inserted;
}

function insertHtmlBlocks(anchor, before, html, created) {
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
  // 後片付け（番兵・末尾の空段落）は差し込みの一部ではないので、変更履歴に残さない。
  inserted = withoutRecording(() => {
    let rest = inserted;
    // 先頭は Rust が置いた番兵の段落（分割した段落の書式を吸わせるためのもの）。捨てる。
    // ただし表と表の間の区切りになる場合は、空の段落として残す（消すと表が 1 つに合体する）。
    if (rest.length > 1 && !isTable(rest[0]) && rest[0].getString() === IMPORT_SENTINEL) {
      if (isTable(rest[1]) && prev !== null && isTable(prev)) {
        rest[0].setString("");
        resetParagraph(rest[0]);
      } else {
        rest[0].dispose();
        rest = rest.slice(1);
      }
    }
    // 取り込みが末尾に空段落を残すことがある。空の段落を後ろから消す。
    // 表の直後で、次も表（か文書の終わり）なら区切りとして残す。
    while (rest.length > 1) {
      const last = rest[rest.length - 1];
      if (isTable(last) || last.getString() !== "") break;
      if (isTable(rest[rest.length - 2]) && (next === null || isTable(next))) break;
      last.dispose();
      rest = rest.slice(0, -1);
    }
    // 末尾の番兵（取り込みの最後の段落。差し込み先の文字書式を吸ったもの）も捨てる。
    const lastIdx = rest.length - 1;
    if (rest.length > 1 && !isTable(rest[lastIdx]) && rest[lastIdx].getString() === IMPORT_SENTINEL) {
      if (isTable(rest[lastIdx - 1]) && (next === null || isTable(next))) {
        rest[lastIdx].setString("");
        resetParagraph(rest[lastIdx]);
      } else {
        rest[lastIdx].dispose();
        rest = rest.slice(0, -1);
      }
    }
    return rest;
  });
  fixImportedStyles(collectParagraphs(inserted));
  for (const b of inserted) if (isTable(b)) padCells(b);
  return inserted;
}

/** 取り込んだ表のセルの内側の余白（取り込みはほぼ 0 にする）。左右 1.9mm・上下 0.8mm（Word の表に近い）。 */
function padCells(table) {
  for (const n of Array.from(table.getCellNames())) {
    try {
      const c = table.getCellByName(n);
      c.setPropertyValue("LeftBorderDistance", long(190));
      c.setPropertyValue("RightBorderDistance", long(190));
      c.setPropertyValue("TopBorderDistance", long(80));
      c.setPropertyValue("BottomBorderDistance", long(80));
    } catch (e) {
      // 余白を持たないセル。
    }
  }
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
    target.setPropertyValue("CharHeight", float(f.size));
    target.setPropertyValue("CharHeightAsian", float(f.size));
  }
  if (f.bold !== undefined) {
    const w = f.bold ? 150 : 100;
    target.setPropertyValue("CharWeight", float(w));
    target.setPropertyValue("CharWeightAsian", float(w));
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
  const a = indexOfBlock(all, refOf(resolved, from, "from"));
  const b = indexOfBlock(all, refOf(resolved, to || from, "to"));
  if (a < 0 || b < 0) fail("block_gone", GONE_MESSAGE);
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

/**
 * 置き換えで隣の表が消えた後、表と表の区切りとして残した空の段落が不要になっていれば消す。
 */
function trimSeparators(inserted) {
  if (inserted.length < 2) return;
  const all = bodyBlocks();
  const first = inserted[0];
  const last = inserted[inserted.length - 1];
  const i = indexOfBlock(all, first);
  if (i >= 0 && !isTable(first) && first.getString() === "" && isTable(inserted[1]) && (i === 0 || !isTable(all[i - 1]))) {
    first.dispose();
  }
  const blocks = bodyBlocks();
  const j = indexOfBlock(blocks, last);
  const after = j >= 0 && j + 1 < blocks.length ? blocks[j + 1] : null;
  if (j >= 0 && !isTable(last) && last.getString() === "" && isTable(inserted[inserted.length - 2]) && after !== null && !isTable(after)) {
    last.dispose();
  }
}

/** ブロックの並びを消す（目次は 1 回だけ消す）。 */
function removeBlocks(range) {
  // 記録中に文書の末尾までの段落を消すときは、直前の段落の終わりからまとめて消す。文書の最後の段落の
  // 区切りは消せないので、段落ごとに消すと承認後に空の段落が 1 つ残る。
  if (recording() && range.every((b) => !isTable(b) && documentIndexOf(b) === null)) {
    const all = bodyBlocks();
    const first = indexOfBlock(all, range[0]);
    const last = indexOfBlock(all, range[range.length - 1]);
    if (first > 0 && last === all.length - 1 && !isTable(all[first - 1])) {
      const cur = DOC.getText().createTextCursorByRange(all[first - 1].getEnd());
      cur.gotoRange(range[range.length - 1].getEnd(), true);
      cur.setString("");
      return;
    }
  }
  const gone = [];
  for (const b of range) {
    const idx = documentIndexOf(b);
    if (idx !== null) {
      if (gone.some((g) => sameObject(g, idx))) continue;
      gone.push(idx);
    }
    removeBlock(b);
  }
}

WOPS.insert_html = (op, resolved, created) => {
  const blank = blankParagraph();
  let anchor = op.anchor === null ? null : refOf(resolved, op.anchor, op.before ? "before" : "after");
  let before = op.before;
  if (op.at === "start") {
    anchor = bodyBlocks()[0];
    before = true;
  }
  const inserted = insertHtml(anchor, before, op.html, created);
  withoutRecording(() => dropEmptyParagraph(blank));
  return { blocks: inserted.length };
};

WOPS.replace_html = (op, resolved, created) => {
  const before = blockRange(resolved, op.from, op.to);
  // 1 段落を、スタイルを書いていない 1 段落で置き換えるなら、元の段落スタイルを引き継ぐ。
  const keepStyle = op.inherit_style && before.length === 1 && !isTable(before[0])
    ? before[0].getPropertyValue("ParaStyleName")
    : null;
  const tables = before.filter(isTable).length;
  // 変更履歴の記録中は、新しい内容を古いブロックの前に入れる（古い段落の削除が新しい段落の区切りに
  // 触れないようにする）。
  const inserted = recording()
    ? insertHtml(before[0], true, op.html, created)
    : insertHtml(before[before.length - 1], false, op.html, created);
  STAGE = "置き換え前のブロックの削除";
  // 差し込みで参照が付け替わることがあるので、範囲は取り直す。
  const range = blockRange(resolved, op.from, op.to);
  if (range.length >= bodyBlocks().length) fail("delete_all", "文書のすべてのブロックは消せません");
  // 消すブロックを指していた ID は、以降の操作では置き換え後の中身を指す（REPLACED の説明）。
  for (const k of Object.keys(resolved)) {
    if (range.some((b) => sameObject(b, resolved[k]))) REPLACED[k] = inserted;
  }
  removeBlocks(range);
  withoutRecording(() => trimSeparators(inserted));
  const out = { removed: range.length, blocks: inserted.length };
  const fresh = inserted.filter((b) => !isTable(b));
  if (keepStyle && inserted.length === 1 && fresh.length === 1 && fresh[0].getPropertyValue("ParaStyleName") !== keepStyle) {
    setParaStyle(fresh[0], keepStyle);
    out.kept_style = keepStyle;
  }
  // 表を作り直すと元の表の見た目（塗り・罫線・列幅）は引き継がれない。結果で知らせる。
  if (tables > 0) out.replaced_tables = tables;
  return out;
};

WOPS.delete = (op, resolved) => {
  const range = blockRange(resolved, op.from, op.to);
  if (range.length >= bodyBlocks().length) fail("delete_all", "文書のすべてのブロックは消せません（最低 1 段落は残ります）");
  removeBlocks(range);
  return { removed: range.length };
};

WOPS.replace_text = (op, resolved) => {
  const hits = findAll(op, resolved);
  if (hits.length === 0) fail("not_found", `「${op.find}」が見つかりません`);
  if (!op.all && hits.length > 1) {
    fail("ambiguous", `「${op.find}」が ${hits.length} か所あります。within で範囲を絞るか、全部なら all: true にしてください`);
  }
  for (const r of hits) r.setString(op.replace);
  return { replaced: hits.length };
};

WOPS.format_text = (op, resolved) => {
  const hits = findAll(op, resolved);
  if (hits.length === 0) fail("not_found", `「${op.find}」が見つかりません`);
  if (!op.all && hits.length > 1) {
    fail("ambiguous", `「${op.find}」が ${hits.length} か所あります。within で範囲を絞るか、全部なら all: true にしてください`);
  }
  for (const r of hits) applyFont(r, op.font);
  return { formatted: hits.length };
};

/** 段落と段落内の文字に直接付いた書式を消し、スタイルどおりにする。 */
const CLEAR_CHAR_PROPS = [
  "CharWeight", "CharWeightAsian", "CharPosture", "CharPostureAsian", "CharColor", "CharHeight", "CharHeightAsian",
  "CharUnderline", "CharStrikeout", "CharFontName", "CharFontNameAsian", "CharBackColor",
];

function clearDirectFormatting(p) {
  const cur = DOC.getText().createTextCursorByRange(p.getStart());
  cur.gotoRange(p.getEnd(), true);
  for (const n of CLEAR_CHAR_PROPS) {
    try {
      cur.setPropertyToDefault(n);
    } catch (e) {
      // 戻せないプロパティ。
    }
  }
  for (const n of RESET_PARA_PROPS) {
    if (n === "NumberingStyleName" || n === "BreakType") continue;
    try {
      p.setPropertyToDefault(n);
    } catch (e) {
      // 戻せないプロパティ。
    }
  }
}

WOPS.format_paragraph = (op, resolved) => {
  const range = blockRange(resolved, op.from, op.to);
  const paras = op.include_tables ? collectParagraphs(range) : range.filter((b) => !isTable(b));
  for (const p of paras) {
    if (op.clear_formatting) clearDirectFormatting(p);
    applyParagraph(p, op);
  }
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
    rememberInserted(fam, op.name);
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
  return bodyPageStyle().style;
}

/** 表紙のページスタイルを本文のものに揃える（用紙・余白）。ヘッダー・フッターは出さない。 */
function syncTitlePage() {
  const body = bodyPageStyle();
  if (!body.titlePage) return;
  const fp = DOC.getStyleFamilies().getByName("PageStyles").getByName(TITLE_PAGE_STYLE);
  for (const n of ["Width", "Height", "IsLandscape", "LeftMargin", "RightMargin", "TopMargin", "BottomMargin"]) {
    fp.setPropertyValue(n, body.style.getPropertyValue(n));
  }
  fp.setPropertyValue("HeaderIsOn", false);
  fp.setPropertyValue("FooterIsOn", false);
}

/** 表紙（1 ページ目にヘッダー・フッターを出さない）。docx では「先頭ページのみ別指定」になる。 */
WOPS.title_page = (op) => {
  const fam = DOC.getStyleFamilies().getByName("PageStyles");
  const body = bodyPageStyle();
  const first = bodyBlocks()[0];
  if (op.enabled) {
    if (!fam.hasByName(TITLE_PAGE_STYLE)) {
      (() => {
        fam.insertByName(TITLE_PAGE_STYLE, DOC.createInstance("com.sun.star.style.PageStyle"));
        rememberInserted(fam, TITLE_PAGE_STYLE);
      })();
    }
    const fp = fam.getByName(TITLE_PAGE_STYLE);
    fp.setPropertyValue("FollowStyle", body.name);
    first.setPropertyValue("PageDescName", TITLE_PAGE_STYLE);
    syncTitlePage();
  } else if (body.titlePage) {
    first.setPropertyValue("PageDescName", body.name);
  }
  return {};
};

/** Writer のページスタイルの属性（失敗時に戻す）。 */
const WRITER_PAGE_PROPS = ["Width", "Height", "IsLandscape", "TopMargin", "RightMargin", "BottomMargin", "LeftMargin"];

WOPS.page_setup = (op) => {
  const st = pageStyle();
  rememberProps(st, WRITER_PAGE_PROPS);
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
  syncTitlePage();
  return {};
};

/** ヘッダー・フッター。pieces は文字とフィールド（ページ番号など）の並び。null で消す。 */
WOPS.header_footer = (op) => {
  const st = pageStyle();
  const key = op.which === "header" ? "Header" : "Footer";
  rememberProps(st, [key + "IsOn"]);
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
  if (op.content.size) cur.setPropertyValue("CharHeight", float(op.content.size));
  return {};
};

/** 本文で最初の見出し（アウトラインの段落）。目次の既定の位置。無ければ null。 */
function firstHeading() {
  for (const b of bodyBlocks()) {
    if (isTable(b) || documentIndexOf(b) !== null) continue;
    try {
      if (b.getPropertyValue("OutlineLevel") > 0) return b;
    } catch (e) {
      // アウトラインを持たない段落。
    }
  }
  return null;
}

WOPS.insert_toc = (op, resolved) => {
  const text = DOC.getText();
  // 目次は文書の先頭（anchor が null）か、指定したブロックの前後に置く。
  let para;
  if (op.anchor !== null) para = emptyParagraphAt(refOf(resolved, op.anchor, op.before ? "before" : "after"), op.before);
  else if (op.at === "end") para = emptyParagraphAt(null, false);
  else if (op.at === "first_heading") para = emptyParagraphAt(firstHeading() || bodyBlocks()[0], true);
  else para = emptyParagraphAt(bodyBlocks()[0], true);
  const toc = DOC.createInstance("com.sun.star.text.ContentIndex");
  const cur = text.createTextCursorByRange(para.getStart());
  text.insertTextContent(cur, toc, false);
  toc.setPropertyValue("Title", op.title);
  toc.setPropertyValue("CreateFromOutline", true);
  toc.setPropertyValue("Level", short(op.levels));
  toc.update();
  withoutRecording(() => dropEmptyParagraph(para));
  // 目次の部品は自分の題（Title）を持つ。直前に同じ題の段落（「目次」の見出しなど）があると題が
  // 二重になり、見出しなら目次の項目にも載る（Gemma 4 の評価で起きた）。その段落を消す。
  const blocks = bodyBlocks();
  const at = blocks.findIndex((b) => {
    const idx = documentIndexOf(b);
    return idx !== null && sameObject(idx, toc);
  });
  const prev = at > 0 ? blocks[at - 1] : null;
  if (prev && !isTable(prev) && op.title && prev.getString().trim() === op.title.trim()) {
    removeBlock(prev);
    toc.update();
    return { removed_duplicate_title: op.title };
  }
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

/** すべての変更履歴を承認・却下する（AI の view の命令として実行する）。 */
WOPS.all_changes = (op) => {
  const before = DOC.getRedlines().createEnumeration().hasMoreElements();
  if (!before) return { changes: 0 };
  const dh = I.frame.DispatchHelper.create(uno.componentContext);
  const frame = DOC.getCurrentController().getFrame();
  dh.executeDispatch(frame, op.accept ? ".uno:AcceptAllTrackedChanges" : ".uno:RejectAllTrackedChanges", "", 0, []);
  if (DOC.getRedlines().createEnumeration().hasMoreElements()) {
    fail("changes_left", "変更履歴を一部処理できませんでした（表の構造の変更などは個別に扱えません）");
  }
  return {};
};

WOPS.track_changes = (op) => {
  // 記録の切り替えは undo に乗らない。一括が失敗したら元の状態へ戻す。
  rememberProps(DOC, ["RecordChanges"]);
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
    RESOLVED = resolved;
    REPLACED = {};
    LAST_TABLE = null;
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
