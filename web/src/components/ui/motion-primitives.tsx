"use client";

/// モーションの共通プリミティブ（UIUX 刷新の土台）。
///
/// ⚠️ パフォーマンス方針（human 明示: アニメで重くしない）:
/// - 既定は **CSS transition/transform**。JS(framer-motion) は「点在する少数の見せ場」だけに使う。
/// - このファイルで JS を使うのは 2 つだけ:
///     `ActiveIndicator`（layoutId によるスライド＝CSS では表現不能）と
///     `FadeSlide`（パネル入場/退場＝AnimatePresence 連携）。
/// - press / lift は **CSS クラス定数**（`PRESSABLE` / `LIFT_ON_HOVER`）で提供し、要素を
///   motion コンポーネントで包まない（大量の要素に JS を載せない）。
/// - 動かすのは transform / opacity / stroke のみ。持続はトークン、reduced-motion は
///   providers の <MotionConfig> と globals.css の @media が二重で停止する。

import * as React from "react";
import { motion, type HTMLMotionProps } from "motion/react";

import { cn } from "@/lib/utils";

/// トークン --ease-standard = cubic-bezier(0.2, 0, 0, 1) と同値。framer は秒指定。
export const EASE_STANDARD = [0.2, 0, 0, 1] as const;
export const DURATION_FAST = 0.12; // --duration-fast (120ms)
export const DURATION_NORMAL = 0.2; // --duration-normal (200ms)

/// 押下フィードバック（CSS）。ボタン等の className に足すだけ。JS 不使用。
export const PRESSABLE =
  "transition-transform duration-[var(--duration-fast)] ease-[var(--ease-standard)] active:scale-[0.97]";

/// ホバーで一段浮く（CSS）。box-shadow の遷移は「ホバー一回きり」に留め常時アニメしない。
export const LIFT_ON_HOVER =
  "transition-[transform,box-shadow] duration-[var(--duration-fast)] ease-[var(--ease-standard)] " +
  "hover:-translate-y-px hover:shadow-md";

/// スライドして追従するアクティブ インジケータ（layoutId）。
/// 同じ `layoutId` を持つ要素は一度に 1 つだけマウントし、アクティブが移ると framer が
/// 位置間をトゥイーンする。呼び出し側は relative な親の中に絶対配置で置く。
/// 例（ナビの左アクセントバー）:
///   {active && <ActiveIndicator layoutId="nav-active" className="absolute left-0 inset-y-1 w-0.5 rounded-full bg-primary" />}
export function ActiveIndicator({
  layoutId,
  className,
  ...props
}: { layoutId: string } & HTMLMotionProps<"span">) {
  return (
    <motion.span
      aria-hidden
      layoutId={layoutId}
      // レイアウトアニメだけ（位置）。spring は控えめ＝跳ねすぎない。
      transition={{ type: "spring", stiffness: 550, damping: 40, mass: 0.7 }}
      className={cn("pointer-events-none", className)}
      {...props}
    />
  );
}

const FADE_SLIDE_OFFSET: Record<"top" | "bottom" | "left" | "right", { x?: number; y?: number }> = {
  top: { y: -6 },
  bottom: { y: 6 },
  left: { x: -6 },
  right: { x: 6 },
};

/// パネル/ツールバーの控えめな入場・退場（fade ＋ わずかな平行移動）。
/// AnimatePresence の子として使うと退場もアニメする。transform/opacity のみ＝軽量。
export function FadeSlide({
  from = "bottom",
  className,
  children,
  ...props
}: { from?: "top" | "bottom" | "left" | "right" } & HTMLMotionProps<"div">) {
  const off = FADE_SLIDE_OFFSET[from];
  return (
    <motion.div
      initial={{ opacity: 0, ...off }}
      animate={{ opacity: 1, x: 0, y: 0 }}
      exit={{ opacity: 0, ...off }}
      transition={{ duration: DURATION_NORMAL, ease: EASE_STANDARD }}
      className={className}
      {...props}
    >
      {children}
    </motion.div>
  );
}
