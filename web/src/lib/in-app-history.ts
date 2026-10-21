/// アプリ内の履歴の深さを記録し、「戻る」で直前の画面がアプリ内かどうかを判定する。
///
/// `window.history.length > 1` は直前のエントリが Keycloak のログイン画面や外部サイトでも
/// 真になるため、`router.back()` でアプリの外へ出てしまう。そこで認証済みシェルの中で
/// 積んだ履歴エントリにだけ、自前の通し番号（このドキュメント内で何件目のアプリ内遷移か）を
/// `history.state` に載せる。番号が 1 以上のエントリなら、1 つ前もアプリ内の画面である。
///
/// - push（Next の `router.push` / `<Link>`）: 直前の番号 +1 を載せる。
/// - replace（`router.replace`・Next 内部の状態更新）: 現在エントリの番号を引き継ぐ。
///   下書き→保存後の URL 置換などで深さを数え違えないため（pathname の変化だけでは区別できない）。
/// - 戻る/進む（popstate）: エントリに保存済みの番号がそのまま現れるので、方向を問わず正しい。
/// - 再読み込み: `history.state` はブラウザが保持するので番号も残る。
/// - 外部から直接開いた最初のエントリ・シェル外で積まれたエントリ: 番号なし（= 0）扱い。

const KEY = "__shikiInAppDepth";

let installed = false;

function depthOf(state: unknown): number {
  if (state && typeof state === "object" && KEY in state) {
    const v = (state as Record<string, unknown>)[KEY];
    if (typeof v === "number" && Number.isFinite(v) && v > 0) return v;
  }
  return 0;
}

function withDepth(data: unknown, depth: number): unknown {
  // Next の内部状態（オブジェクト）を壊さないよう、キーを足した浅いコピーを渡す。
  if (data === null || data === undefined) return { [KEY]: depth };
  if (typeof data === "object" && !Array.isArray(data)) return { ...(data as object), [KEY]: depth };
  return data;
}

/// 認証済みシェルのマウント時に 1 度だけ呼ぶ（多重呼び出しは無視）。以降の push/replace を記録する。
/// Next 自身も `history.pushState` を包むが、どちらも前の実装を呼び継ぐので順序は問わない。
export function installInAppHistoryTracking(): void {
  if (installed || typeof window === "undefined") return;
  installed = true;
  const { history } = window;
  const push = history.pushState.bind(history);
  const replace = history.replaceState.bind(history);
  history.pushState = function pushState(data, unused, url) {
    push(withDepth(data, depthOf(history.state) + 1), unused, url);
  };
  history.replaceState = function replaceState(data, unused, url) {
    const depth = depthOf(history.state);
    replace(depth > 0 && depthOf(data) === 0 ? withDepth(data, depth) : data, unused, url);
  };
}

/// 1 つ前の履歴エントリがアプリ内の画面か（`router.back()` してもアプリの外へ出ないか）。
export function canGoBackInApp(): boolean {
  if (typeof window === "undefined") return false;
  return depthOf(window.history.state) > 0;
}
