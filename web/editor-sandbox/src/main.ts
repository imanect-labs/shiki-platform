/// GrapesJS スライドエディタ（砂箱バンドル本体・Task 11.2・design §4.8.3）。
///
/// 役割は「選択中の 1 枚の HTML を編集する」ことに限定する。スライド一覧・並べ替え・
/// Yjs 同期・保存はすべて親（アプリオリジン）の責務。編集結果はデバウンスして
/// `slide:changed` で親へ返し、親が Yjs へ差分適用する。
///
/// 設定方針:
/// - `avoidInlineStyle: false` — スタイルはインライン style 属性に書く（スライド HTML の
///   自己完結性・サニタイズ・pptx 変換のため。CSS ルールの別持ちをしない）
/// - キャンバスは 1280×720 固定（ビューア/pptx 計測と同一の論理キャンバス）
/// - スクリプト実行はしない（GrapesJS 既定で component script は無効・allowScripts 未設定）

import grapesjs, { type Editor } from "grapesjs";
import { acceptPort, type SandboxMessage } from "./bridge";

/// ビューア（slide-frame.tsx）と揃えた基本タイポグラフィ。キャンバス内にのみ適用される。
const CANVAS_CSS = `
  *, *::before, *::after { box-sizing: border-box; }
  html, body { margin: 0; width: 1280px; height: 720px; overflow: hidden; }
  body {
    font-family: "Hiragino Sans", "Noto Sans JP", "Yu Gothic", system-ui, sans-serif;
    color: #1a1a1a; background: #ffffff; padding: 0; line-height: 1.5;
  }
  /* GrapesJS はコンテンツを wrapper 要素に入れる。ビューア（body 直下）と同じ
     レイアウト（余白＋垂直センタリング）を wrapper 側に適用して WYSIWYG を揃える。 */
  [data-gjs-type="wrapper"] {
    width: 100%; height: 720px;
    display: flex; flex-direction: column; justify-content: center;
    padding: 72px 96px;
  }
  h1 { font-size: 64px; font-weight: 700; margin: 0 0 24px; letter-spacing: -0.01em; }
  h2 { font-size: 44px; font-weight: 700; margin: 0 0 20px; }
  h3 { font-size: 32px; font-weight: 600; margin: 0 0 16px; }
  p, li { font-size: 26px; margin: 0 0 12px; }
  ul, ol { margin: 0 0 12px; padding-left: 1.4em; }
  table { border-collapse: collapse; font-size: 22px; }
  td, th { border: 1px solid #d4d4d4; padding: 8px 14px; text-align: left; }
  th { background: #f5f5f4; }
  img { max-width: 100%; }
  blockquote { border-left: 4px solid #d4d4d4; margin: 0 0 12px; padding: 4px 0 4px 20px; color: #555; }
  pre, code { font-family: ui-monospace, "SFMono-Regular", monospace; font-size: 22px; }
`;

/// 変換可能サブセット（pptx エクスポート・design §4.8.3）に寄せた基本ブロック。
const BLOCKS: { id: string; label: string; content: string }[] = [
  { id: "heading", label: "見出し", content: "<h2>見出し</h2>" },
  { id: "text", label: "テキスト", content: "<p>テキストを入力</p>" },
  { id: "list", label: "箇条書き", content: "<ul><li>項目 1</li><li>項目 2</li></ul>" },
  {
    id: "two-col",
    label: "2 カラム",
    content:
      '<div style="display:flex; gap:48px"><div style="flex:1"><h3>左</h3><p>内容</p></div><div style="flex:1"><h3>右</h3><p>内容</p></div></div>',
  },
  {
    id: "box",
    label: "ボックス",
    content:
      '<div style="background:#f5f5f4; border-radius:16px; padding:32px"><p>強調したい内容</p></div>',
  },
  {
    id: "table",
    label: "表",
    content:
      "<table><thead><tr><th>項目</th><th>値</th></tr></thead><tbody><tr><td>A</td><td>1</td></tr><tr><td>B</td><td>2</td></tr></tbody></table>",
  },
];

/// 編集結果の親への通知デバウンス（タイプ中の全キーで送らない）。
const CHANGE_DEBOUNCE_MS = 300;

function boot() {
  const editor: Editor = grapesjs.init({
    container: "#gjs",
    height: "100%",
    width: "auto",
    fromElement: false,
    storageManager: false,
    undoManager: { trackSelection: false },
    avoidInlineStyle: false,
    // アイコン CSS はバンドルへ同梱済み（CDN 読み込みを発生させない・CSP で遮断される）。
    cssIcons: "",
    canvas: { styles: [], scripts: [] },
    blockManager: { blocks: BLOCKS.map((b) => ({ ...b, select: true })) },
    deviceManager: { devices: [{ id: "slide", name: "スライド", width: "1280px", height: "720px" }] },
  });
  editor.setDevice("slide");

  // キャンバス（スライド内）へ基本タイポグラフィを注入する。フレームは device 切替等で
  // 再生成され得るため、load 系イベントごとに冪等に差し込む。
  const injectCanvasCss = () => {
    const doc = editor.Canvas.getDocument();
    if (!doc || doc.getElementById("shiki-slide-base")) return;
    const style = doc.createElement("style");
    style.id = "shiki-slide-base";
    style.textContent = CANVAS_CSS;
    doc.head.appendChild(style);
  };
  editor.on("load canvas:frame:load", injectCanvasCss);

  // キャンバスを表示領域へフィットさせる（1280×720 の論理キャンバスを等倍縮尺）。
  const fitCanvas = () => {
    const container = editor.Canvas.getElement();
    if (!container) return;
    const scale = Math.min(container.clientWidth / 1280, container.clientHeight / 720) * 0.96;
    if (scale > 0 && Number.isFinite(scale)) {
      editor.Canvas.setZoom(scale * 100);
    }
  };
  editor.on("load", fitCanvas);
  window.addEventListener("resize", fitCanvas);

  // デバッグ・e2e 用の限定フック（砂箱オリジン内のみ・親からは不可達）。
  (window as unknown as { __shikiEditor?: unknown }).__shikiEditor = editor;

  let port: MessagePort | null = null;
  let currentId: string | null = null;
  // 親からのロード適用中は change を親へ返さない（エコー抑制の砂箱側半分）。
  let applyingRemote = false;
  let pending: number | null = null;

  const send = (msg: SandboxMessage) => port?.postMessage(msg);

  /// getHtml はラッパ（body タグ）込みで返るため剥がす（スライド HTML は body 直下の断片）。
  const currentHtml = () =>
    editor
      .getHtml()
      .replace(/^\s*<body[^>]*>/, "")
      .replace(/<\/body>\s*$/, "");

  const scheduleChange = () => {
    if (applyingRemote || !currentId) return;
    if (pending !== null) window.clearTimeout(pending);
    pending = window.setTimeout(() => {
      pending = null;
      if (!currentId) return;
      send({ type: "slide:changed", id: currentId, html: currentHtml() });
    }, CHANGE_DEBOUNCE_MS);
  };

  // コンポーネント/スタイルの全変更を 1 本のフックで拾う（rte:disable = テキスト編集の確定）。
  editor.on("component:add component:remove component:update style:change rte:disable", scheduleChange);

  /// 未確定の編集を確定して親へ送る（スライド切替時のデータ喪失防止）。
  /// RTE（テキスト編集中）はコンポーネントモデルへの反映が select 解除後の tick で
  /// 走るため、解除 → 1 tick 待ち → 読み出しの順にする（非同期）。
  const flushCurrent = async () => {
    if (pending !== null) {
      window.clearTimeout(pending);
      pending = null;
    }
    if (!currentId || applyingRemote) return;
    // RTE（テキスト編集中）はモデル反映が view.disableEditing() 経由でのみ確定する
    // （blur や select 解除では確定しない・実測）。
    const em = editor.getModel() as unknown as {
      getEditing?: () => { view?: { disableEditing?: () => Promise<void> } } | null;
    };
    const editing = em.getEditing?.();
    if (editing?.view?.disableEditing) {
      await editing.view.disableEditing();
    }
    editor.select(undefined);
    await new Promise((r) => window.setTimeout(r, 30));
    if (currentId) send({ type: "slide:changed", id: currentId, html: currentHtml() });
  };

  // メッセージは到着順に直列処理する（flush の await 中に次の load を混ぜない）。
  let queue: Promise<void> = Promise.resolve();
  void acceptPort((msg) => {
    queue = queue.then(() => handle(msg));
  }).then((p) => {
    port = p;
    send({ type: "ready" });
  });

  async function handle(msg: Parameters<Parameters<typeof acceptPort>[0]>[0]) {
    switch (msg.type) {
      case "slide:load": {
        // 別スライドへの切替なら、直前のスライドの未確定編集を先に確定・送信する。
        if (currentId && currentId !== msg.id) await flushCurrent();
        applyingRemote = true;
        try {
          if (pending !== null) {
            window.clearTimeout(pending);
            pending = null;
          }
          currentId = msg.id;
          editor.setComponents(msg.html);
          // 閲覧のみでは編集ジェスチャを無効化する（強制はサーバ側の viewer 拒否）。
          editor.getModel().set("editing", false);
          const body = editor.Canvas.getBody();
          if (body) body.setAttribute("contenteditable", msg.editable ? "true" : "false");
        } finally {
          // setComponents 由来の change イベントが同期で流れ終わってから解除する。
          window.setTimeout(() => {
            applyingRemote = false;
          }, 0);
        }
        break;
      }
      case "deck:empty": {
        currentId = null;
        editor.setComponents("");
        break;
      }
      default:
    }
  }
}

boot();
