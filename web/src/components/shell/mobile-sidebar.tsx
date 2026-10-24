"use client";

import * as React from "react";
import * as DialogPrimitive from "@radix-ui/react-dialog";
import { VisuallyHidden } from "@radix-ui/react-visually-hidden";

import { cn } from "@/lib/utils";
import { useSidebar } from "./sidebar/sidebar-context";
import { SidebarContent } from "./sidebar/sidebar";

/// 左スワイプで閉じる判定: 軸を決めるまでの遊び・閉じる距離・素早いフリックの距離と時間。
const AXIS_SLOP_PX = 8;
const CLOSE_DISTANCE_PX = 80;
const FLICK_DISTANCE_PX = 40;
const FLICK_MS = 250;

/// ドロワを指で左へ引いて閉じる（簡易版・依存を増やさない）。
///
/// 最初の数 px で縦横を決め、横のときだけドロワを指に追従させる（縦は履歴のスクロールに任せる）。
/// 閉じるときは追従した位置をそのまま残す。退場アニメーションは終点だけを持つので、
/// その位置から左へ抜ける（引き戻されてから閉じるちらつきが出ない）。
function useSwipeToClose(onClose: () => void) {
  const ref = React.useRef<HTMLDivElement | null>(null);
  const drag = React.useRef<{
    x: number;
    y: number;
    t: number;
    axis: "x" | "y" | null;
    dx: number;
  } | null>(null);

  const onTouchStart = (e: React.TouchEvent) => {
    if (e.touches.length !== 1) {
      drag.current = null;
      return;
    }
    const t = e.touches[0];
    drag.current = { x: t.clientX, y: t.clientY, t: e.timeStamp, axis: null, dx: 0 };
  };

  const onTouchMove = (e: React.TouchEvent) => {
    const d = drag.current;
    const el = ref.current;
    const t = e.touches[0];
    if (!d || !el || !t) return;
    const dx = t.clientX - d.x;
    const dy = t.clientY - d.y;
    if (d.axis === null) {
      if (Math.abs(dx) < AXIS_SLOP_PX && Math.abs(dy) < AXIS_SLOP_PX) return;
      d.axis = Math.abs(dx) > Math.abs(dy) ? "x" : "y";
    }
    if (d.axis !== "x") return;
    d.dx = Math.min(0, dx);
    el.style.transition = "none";
    el.style.transform = `translateX(${d.dx}px)`;
  };

  const onTouchEnd = (e: React.TouchEvent) => {
    const d = drag.current;
    const el = ref.current;
    drag.current = null;
    if (!d || !el || d.axis !== "x") return;
    const flick = d.dx < -FLICK_DISTANCE_PX && e.timeStamp - d.t < FLICK_MS;
    if (d.dx < -CLOSE_DISTANCE_PX || flick) {
      el.style.transition = "";
      onClose();
      return;
    }
    // 閉じない: 元の位置へ戻す。
    el.style.transition = "transform 160ms var(--ease-standard)";
    el.style.transform = "";
  };

  return { ref, handlers: { onTouchStart, onTouchMove, onTouchEnd, onTouchCancel: onTouchEnd } };
}

/// ドロワの上のモーダルな層（アカウントメニュー等）を閉じるための押下は、それ以上何もさせない。
///
/// 層が開いている間、ドロワ本体は Radix が pointer-events: none にする。メニューは pointerdown で
/// 閉じて本体の pointer-events が戻るため、続く click が (1) 指の下の要素（ロゴ・履歴のリンク）に
/// 届いて遷移したり、(2) click まで外側判定を遅らせる Dialog に「ドロワの外を押した」と
/// 判定されたりして、メニューと一緒にドロワまで閉じてしまう。
///
/// 本体が押せない間は、層の外（ドロワ内でも外でも）を押すと当たり先は背面のオーバーレイになる。
/// 押した瞬間（capture）に「本体が押せない状態でオーバーレイに当たった」なら層を閉じるための
/// 押下とみなし、その click を握りつぶし、Dialog の外側判定も取り消す（オーバーレイへの click は
/// Radix が握りつぶされても閉じる扱いにするため、両方が要る）。層の中（メニュー項目）は対象外。
/// `pressed` は Dialog.Content の onPointerDownOutside で見る。次の押下で改めて判定する。
function useNestedLayerDismissPress(
  open: boolean,
  contentRef: React.RefObject<HTMLDivElement | null>,
) {
  const overlayRef = React.useRef<HTMLDivElement | null>(null);
  const pressed = React.useRef(false);
  React.useEffect(() => {
    if (!open) return;
    const onPointerDown = (e: PointerEvent) => {
      const el = contentRef.current;
      pressed.current =
        !!el &&
        e.target === overlayRef.current &&
        getComputedStyle(el).pointerEvents === "none";
    };
    const onClick = (e: MouseEvent) => {
      if (!pressed.current) return;
      e.preventDefault();
      e.stopPropagation();
    };
    document.addEventListener("pointerdown", onPointerDown, true);
    document.addEventListener("click", onClick, true);
    return () => {
      pressed.current = false;
      document.removeEventListener("pointerdown", onPointerDown, true);
      document.removeEventListener("click", onClick, true);
    };
  }, [open, contentRef]);
  return { overlayRef, pressed };
}

/// モバイル（< md）でのサイドバー。左からスライドする浮いたドロワ（角丸・PC の aside と同じ素材）。
/// Radix Dialog でフォーカストラップ・Esc・スクロールロック・aria-modal を担保する。
export function MobileSidebar() {
  const { mobileOpen, setMobileOpen } = useSidebar();
  const close = React.useCallback(() => setMobileOpen(false), [setMobileOpen]);
  const swipe = useSwipeToClose(close);
  const nestedLayerDismissPress = useNestedLayerDismissPress(mobileOpen, swipe.ref);

  return (
    <DialogPrimitive.Root open={mobileOpen} onOpenChange={setMobileOpen}>
      <DialogPrimitive.Portal>
        <DialogPrimitive.Overlay
          ref={nestedLayerDismissPress.overlayRef}
          className={cn(
            "fixed inset-0 z-50 bg-black/40 backdrop-blur-[1px] md:hidden",
            "data-[state=open]:animate-in data-[state=open]:fade-in-0 data-[state=closed]:animate-out data-[state=closed]:fade-out-0",
          )}
        />
        <DialogPrimitive.Content
          ref={swipe.ref}
          {...swipe.handlers}
          onPointerDownOutside={(e) => {
            if (nestedLayerDismissPress.pressed.current) e.preventDefault();
          }}
          className={cn(
            // PC と同じ「浮いたパネル」: 画面端から 8px 離し（ノッチ・ホームバーの内側）、角丸で切る。
            "fixed left-2 top-[max(0.5rem,env(safe-area-inset-top))] bottom-[max(0.5rem,env(safe-area-inset-bottom))] z-50 w-[min(330px,calc(88vw-0.5rem))]",
            "overflow-hidden rounded-xl border border-sidebar-border bg-sidebar shadow-lg outline-none md:hidden",
            // 横の指の動きはスワイプで閉じる処理に回し、縦のスクロールはブラウザに任せる。
            "touch-pan-y",
            // 左に 8px 空けているので、自分の幅より少し多く動かして画面外から出入りさせる。
            "data-[state=open]:animate-in data-[state=open]:slide-in-from-left-[110%] data-[state=closed]:animate-out data-[state=closed]:slide-out-to-left-[110%]",
            "duration-200",
          )}
        >
          <VisuallyHidden>
            <DialogPrimitive.Title>ナビゲーション</DialogPrimitive.Title>
            <DialogPrimitive.Description>
              サイドバーのメニュー
            </DialogPrimitive.Description>
          </VisuallyHidden>
          {/* モバイルは常に展開・折りたたみトグル無し（閉じる ×）・遷移で自動クローズ */}
          <SidebarContent collapsed={false} mobile onNavigate={close} />
        </DialogPrimitive.Content>
      </DialogPrimitive.Portal>
    </DialogPrimitive.Root>
  );
}
