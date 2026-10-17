// Shiki Office Engine — 共通部（#725・PIT-68）。
//
// このディレクトリの JS は kit（LibreOffice）の中で QuickJS＋jsuno として走る。
// Rust（office::engine::script）が core.js → 各モジュール → 末尾の呼び出しの順に連結し、
// `function (req) { ... }` 1 個として executescript に渡す。LLM の出力は `req`（JSON）として
// だけ入り、ここのソースへ文字列として混ざることはない。
//
// jsuno の癖（実機で確認）:
// - UNO の enum はオブジェクトで返る。String(v) は "com.sun.star.table.CellHoriJustify.CENTER"。
// - インタフェースのメソッドは動的に解決される（Object.keys には出ない）。サービスの属性は
//   getPropertyValue で読む（例: XConditionalFormat の範囲は "Range" プロパティ）。
// - long 型のプロパティへ JS の数値を入れると型が合わないことがある → new uno.Any(uno.type.long, n)。
// - UNO の例外は { Message, Context } のオブジェクトで飛ぶ（Error ではない）。
// - kit を落とす操作がある（不正な tile 要求など）。ここでは描画系の命令を一切使わない。

const I = uno.idl.com.sun.star;

/**
 * この kit が開いている文書。kit は 1 プロセス 1 文書なので通常は getCurrentComponent で足りるが、
 * 読み込み直後などに null を返すことがある（実機で確認）ので、開いている部品の一覧から取り直す。
 */
const DOC = (() => {
  const desktop = I.frame.Desktop.create(uno.componentContext);
  const current = desktop.getCurrentComponent();
  if (current) return current;
  const en = desktop.getComponents().createEnumeration();
  return en.hasMoreElements() ? en.nextElement() : null;
})();

/** action 名 → ハンドラ。各モジュールが登録する。 */
const HANDLERS = {};

/** いま実行中の段階（UNO 例外はメッセージが空のことが多いので、どこで落ちたかを残す）。 */
let STAGE = "";

/** エンジンの失敗（コード・利用者向けメッセージ・補足）。 */
class EngineError extends Error {
  constructor(code, message, extra) {
    super(message);
    this.code = code;
    this.extra = extra || null;
  }
}

function fail(code, message, extra) {
  throw new EngineError(code, message, extra);
}

/** enum を末尾の名前にする（"…CellHoriJustify.CENTER" → "CENTER"）。 */
function enumName(v) {
  if (v === null || v === undefined) return null;
  const s = String(v);
  const i = s.lastIndexOf(".");
  return i >= 0 ? s.slice(i + 1) : s;
}

function long(n) {
  return new uno.Any(uno.type.long, n | 0);
}

function short(n) {
  return new uno.Any(uno.type.short, n | 0);
}

/** UNO の色（0xRRGGBB・自動は -1）→ "#RRGGBB"。自動・透過は null。 */
function colorToHex(n) {
  if (n === null || n === undefined || n < 0) return null;
  return "#" + (n & 0xffffff).toString(16).padStart(6, "0").toUpperCase();
}

/** "#RRGGBB" → UNO の色。null は「自動」(-1)。形式は Rust 側で検証済み。 */
function hexToColor(h) {
  if (h === null || h === undefined) return -1;
  return parseInt(String(h).replace("#", ""), 16);
}

function propertyValue(name, value) {
  const p = new I.beans.PropertyValue();
  p.Name = name;
  p.Value = value;
  return p;
}

/** 文書の種類。 */
function docKind() {
  if (DOC.supportsService("com.sun.star.sheet.SpreadsheetDocument")) return "calc";
  if (DOC.supportsService("com.sun.star.text.TextDocument")) return "writer";
  if (DOC.supportsService("com.sun.star.presentation.PresentationDocument")) return "impress";
  return "other";
}

function requireKind(kind) {
  const k = docKind();
  if (k !== kind) fail("wrong_document_kind", `この操作は ${kind} 用です（開いている文書は ${k}）`);
}

/** UNO の例外・JS の例外を { code, message, stage } にする。 */
function describeError(e) {
  if (e instanceof EngineError) {
    return Object.assign({ code: e.code, message: e.message, stage: STAGE }, e.extra || {});
  }
  let message = "";
  if (e && typeof e === "object") message = e.Message || e.message || "";
  if (!message) message = String(e);
  if (message === "[object Object]") message = "";
  return { code: "uno_error", message: message || "LibreOffice が操作を拒否しました", stage: STAGE };
}

/**
 * 一括適用を 1 つの undo 単位にまとめ、途中で失敗したら丸ごと取り消す（アトミック）。
 *
 * 取り消してよいのは「この文脈で記録が増えたとき」だけ。何も記録されずに失敗したときに
 * undo すると、直前の別の操作（人や前回の AI の編集）を取り消してしまう。
 */
function atomically(label, fn) {
  const um = DOC.getUndoManager();
  const before = um.getAllUndoActionTitles().length;
  um.enterUndoContext(label);
  let ok = false;
  try {
    const r = fn();
    ok = true;
    return r;
  } finally {
    // 文脈を閉じる処理が失敗しても取り消しは試みる（失敗した UNO 呼び出しの後に起き得る）。
    try {
      um.leaveUndoContext();
    } catch (e) {
      STAGE = STAGE + "（undo 文脈を閉じられませんでした）";
    }
    if (!ok) {
      const after = um.getAllUndoActionTitles().length;
      if (after > before) {
        try {
          um.undo();
        } catch (e) {
          // 取り消しに失敗したら、それ自体を報告に残す（黙って半端な状態にしない）。
          STAGE = STAGE + "（取り消しにも失敗）";
        }
      }
    }
  }
}

/** エントリポイント。戻り値は必ず { ok, result | error }。 */
function __shiki_main(req) {
  if (!DOC) return { ok: false, error: { code: "no_document", message: "文書がまだ開かれていません" } };
  const handler = HANDLERS[req && req.action];
  if (!handler) {
    return { ok: false, error: { code: "unknown_action", message: `未知の action: ${req && req.action}` } };
  }
  try {
    return { ok: true, result: handler(req) };
  } catch (e) {
    return { ok: false, error: describeError(e) };
  }
}

HANDLERS["engine.ping"] = () => ({ kind: docKind(), engine: 1 });
