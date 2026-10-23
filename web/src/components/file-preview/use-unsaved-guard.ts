"use client";

/// 未保存の編集があるときに画面を離れる操作を止める（#827）。
///
/// - タブを閉じる・再読み込み: ブラウザの確認を出す（`beforeunload`）。
/// - アプリ内のリンク（サイドバーなど）: クリックを止めて `confirm(go)` に回す。`go` を呼ぶと移動する。
/// - ブラウザの「戻る」: 編集中は同じ URL の履歴を 1 つ積んでおき、戻るでそれが消費されたら
///   （＝まだ同じ画面にいる）、未保存なら積み直して確認を出す。未保存でなければそのままもう 1 つ戻る。
///   App Router は遷移を止める仕組みを持たないため、履歴を 1 つ余分に持つ形で受け止める。
import * as React from "react";
import { useRouter } from "next/navigation";

/// 積んだ履歴の目印（Next の pushState パッチは独自キーを残したまま内部状態を足す）。
const GUARD_KEY = "__shikiUnsavedGuard";

export function useUnsavedGuard({
  editing,
  dirty,
  confirm,
}: {
  editing: boolean;
  dirty: boolean;
  confirm: (go: () => void) => void;
}): void {
  const router = useRouter();
  const confirmRef = React.useRef(confirm);
  confirmRef.current = confirm;
  const dirtyRef = React.useRef(dirty);
  dirtyRef.current = dirty;

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

  React.useEffect(() => {
    if (!editing) return;
    const href = window.location.href;
    const pushGuard = () => window.history.pushState({ [GUARD_KEY]: true }, "", href);
    pushGuard();
    let leaving = false;
    const onPopState = () => {
      // 目印の上から戻った（同じ画面に留まっている）ときだけ扱う。進む・別の画面への戻りは触らない。
      if (leaving || window.location.href !== href || isGuard(window.history.state)) return;
      if (!dirtyRef.current) {
        leaving = true;
        window.history.back();
        return;
      }
      pushGuard();
      confirmRef.current(() => {
        leaving = true;
        // 積み直した目印と、元の画面の 2 つを戻る。
        window.history.go(-2);
      });
    };
    window.addEventListener("popstate", onPopState);
    return () => {
      window.removeEventListener("popstate", onPopState);
      // 編集を終えて同じ画面に残るなら、積んだ目印を外す（戻るが 1 回で済むように）。
      if (!leaving && window.location.href === href && isGuard(window.history.state)) window.history.back();
    };
  }, [editing]);
}

function isGuard(state: unknown): boolean {
  return typeof state === "object" && state != null && GUARD_KEY in state;
}
