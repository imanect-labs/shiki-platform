"use client";

/// 未保存の編集があるときに画面を離れる操作を止める（#827）。
///
/// - タブを閉じる・再読み込み: ブラウザの確認を出す（`beforeunload`）。
/// - アプリ内のリンク（サイドバーなど）: クリックを止めて `confirm(go)` に回す。`go` を呼ぶと移動する。
///
/// ブラウザの「戻る」は止められない（App Router に遷移を止める仕組みが無い）。
import * as React from "react";
import { useRouter } from "next/navigation";

export function useUnsavedGuard(dirty: boolean, confirm: (go: () => void) => void): void {
  const router = useRouter();
  const confirmRef = React.useRef(confirm);
  confirmRef.current = confirm;

  React.useEffect(() => {
    if (!dirty) return;
    const onBeforeUnload = (e: BeforeUnloadEvent) => {
      e.preventDefault();
      // 古いブラウザは returnValue が要る。
      e.returnValue = "";
    };
    // React のハンドラ（next/link の遷移）より先に、捕捉相で止める。
    const onClick = (e: MouseEvent) => {
      if (e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
      const a = (e.target as Element | null)?.closest?.("a[href]");
      if (!(a instanceof HTMLAnchorElement)) return;
      if (a.target && a.target !== "_self") return;
      if (a.hasAttribute("download")) return;
      const url = new URL(a.href, window.location.href);
      if (url.origin !== window.location.origin) return;
      if (url.pathname === window.location.pathname && url.search === window.location.search) return;
      e.preventDefault();
      e.stopPropagation();
      confirmRef.current(() => router.push(url.pathname + url.search + url.hash));
    };
    window.addEventListener("beforeunload", onBeforeUnload);
    document.addEventListener("click", onClick, true);
    return () => {
      window.removeEventListener("beforeunload", onBeforeUnload);
      document.removeEventListener("click", onClick, true);
    };
  }, [dirty, router]);
}
