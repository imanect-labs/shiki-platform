"use client";

import * as React from "react";

/// 画面下端に貼り付くシートを、ソフトキーボードの上（見えている領域の下端）まで持ち上げる。
///
/// iOS はキーボードを出しても layout viewport が縮まず（viewport の interactive-widget も効かない）、
/// `position: fixed; bottom: 0` のシートはキーボードの裏に入って入力欄が隠れる。visualViewport の
/// 下端と layout viewport の下端の差を `--keyboard-inset`、見えている高さを `--visible-height` として
/// 要素に書き込み、CSS 側で bottom / max-height に使う（Android は resizes-content で layout viewport
/// ごと縮むので差は 0 になる）。スクロールのたびに描画し直さないよう、state ではなく style に直接書く。
///
/// 返り値は要素に渡す ref。`enabled` が偽のとき（PC の中央ダイアログ等）は何も書かない。
export function useKeyboardInset<T extends HTMLElement>(enabled: () => boolean): React.RefCallback<T> {
  const [node, setNode] = React.useState<T | null>(null);
  const enabledRef = React.useRef(enabled);
  enabledRef.current = enabled;
  React.useEffect(() => {
    const vv = typeof window !== "undefined" ? window.visualViewport : null;
    if (!node || !vv) return;
    const update = () => {
      // 開いたまま回転して幅が変わることがあるので、毎回判定する（PC の中央ダイアログでは書かない）。
      if (!enabledRef.current()) {
        node.style.removeProperty("--keyboard-inset");
        node.style.removeProperty("--visible-height");
        return;
      }
      // note-toolbar と同じく、見えている領域の下端に直接合わせる（innerHeight の挙動に依存しない）。
      const inset = Math.max(0, document.documentElement.clientHeight - (vv.offsetTop + vv.height));
      node.style.setProperty("--keyboard-inset", `${Math.round(inset)}px`);
      node.style.setProperty("--visible-height", `${Math.round(vv.height)}px`);
    };
    update();
    vv.addEventListener("resize", update);
    vv.addEventListener("scroll", update);
    return () => {
      vv.removeEventListener("resize", update);
      vv.removeEventListener("scroll", update);
      node.style.removeProperty("--keyboard-inset");
      node.style.removeProperty("--visible-height");
    };
  }, [node]);
  return setNode;
}
