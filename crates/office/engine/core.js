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
//   ただし uno.Any で包むのは any 型の引数（setPropertyValue の値・PropertyValue.Value）だけ。
//   型の決まった引数（insertNewByName の short など）へ渡すと 26.04.5 では数値として読まれず
//   「Bad UNO short value nan」になる（実機で確認）。そこへは JS の整数をそのまま渡す。
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

/**
 * float 型の属性（CharHeight・CharWeight など）の値。グラフの題などは値の型を変換せずにそのまま持ち、
 * JS の整数のまま入れると xlsx への書き出しが「The file could not be written」で失敗する
 * （26.04.5 で実測。セル・段落は変換されるが、どこでも同じ書き方にそろえる）。
 */
function float(n) {
  return new uno.Any(uno.type.float, Number(n));
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
  // 消えた（置き換わった）オブジェクトへの参照。同じ一括の中で削除・置換したブロックの ID を使ったとき。
  if (/disposed or invalid|DisposedException/i.test(message)) {
    return {
      code: "block_gone",
      message: "対象のブロックはこの一括の中で既に消えたか置き換わっています（delete・replace の後に元の ID を使った可能性）。" +
        "その操作は次の呼び出しに分け、結果に返るブロック ID を使ってください",
      stage: STAGE,
    };
  }
  return { code: "uno_error", message: message || "LibreOffice が操作を拒否しました", stage: STAGE };
}

/**
 * undo に記録されない変更（変更履歴の記録の切り替え・ページスタイル・作ったスタイルなど）を、
 * 一括が失敗したときに戻す手続き。操作が変更の前に `onRollback(() => 元へ戻す)` で積む。
 */
let ROLLBACK = [];

function onRollback(fn) {
  ROLLBACK.push(fn);
}

/** obj の属性 names の今の値を覚え、一括が失敗したら戻す（undo に乗らない属性の変更の前に呼ぶ）。 */
function rememberProps(obj, names) {
  const saved = [];
  for (const n of names) {
    try {
      saved.push([n, obj.getPropertyValue(n)]);
    } catch (e) {
      // その属性を持たない（版・種類による）。
    }
  }
  onRollback(() => {
    for (const [n, v] of saved) obj.setPropertyValue(n, v);
  });
}

/** 名前 name の要素を family へ足した後に呼ぶ。一括が失敗したら取り除く。 */
function rememberInserted(family, name) {
  onRollback(() => {
    if (family.hasByName(name)) family.removeByName(name);
  });
}

/** undo の一覧（新しい順の題名）が、文脈を閉じた後に変わったか。 */
function undoRecorded(before, after) {
  if (after.length !== before.length) return after.length > before.length;
  // 段数の上限（既定 100）に達していると、新しい記録が最古の 1 件を押し出して件数は変わらない。
  // 題名の並びがずれたかで見る。
  for (let i = 0; i < after.length; i++) if (after[i] !== before[i]) return true;
  return false;
}

/**
 * 一括適用を 1 つの undo 単位にまとめ、途中で失敗したら丸ごと取り消す（アトミック）。
 *
 * 取り消してよいのは「この文脈で記録が増えたとき」だけ。何も記録されずに失敗したときに
 * undo すると、直前の別の操作（人や前回の AI の編集）を取り消してしまう。
 * undo に乗らない変更は [`onRollback`] で積んだ手続きで、逆順に戻す。
 */
function atomically(label, fn) {
  const um = DOC.getUndoManager();
  const before = Array.from(um.getAllUndoActionTitles());
  ROLLBACK = [];
  um.enterUndoContext(label);
  let ok = false;
  try {
    const r = fn();
    ok = true;
    // スタイル・ページ設定だけの変更は文書の「変更あり」にならないことがあり、そのままだと
    // 「変更が無ければ保存しない」保存で捨てられる（実機で確認）。一括が成功したら変更ありにする。
    try {
      DOC.setModified(true);
    } catch (e) {
      // 変更の印を持たない文書。
    }
    return r;
  } finally {
    // 文脈を閉じる処理が失敗しても取り消しは試みる（失敗した UNO 呼び出しの後に起き得る）。
    try {
      um.leaveUndoContext();
    } catch (e) {
      STAGE = STAGE + "（undo 文脈を閉じられませんでした）";
    }
    if (!ok) {
      if (undoRecorded(before, Array.from(um.getAllUndoActionTitles()))) {
        try {
          um.undo();
        } catch (e) {
          // 取り消しに失敗したら、それ自体を報告に残す（黙って半端な状態にしない）。
          STAGE = STAGE + "（取り消しにも失敗）";
        }
      }
      for (const back of ROLLBACK.reverse()) {
        try {
          back();
        } catch (e) {
          STAGE = STAGE + "（一部の設定を戻せませんでした）";
        }
      }
    }
    ROLLBACK = [];
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
