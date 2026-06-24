import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

/// クラス名を結合しつつ Tailwind の競合（例: `px-2` と `px-4`）を後勝ちで解決する。
/// 全コンポーネントの className 合成はこの 1 箇所を経由させる（正本）。
export function cn(...inputs: ClassValue[]): string {
  return twMerge(clsx(inputs));
}

/// 表示名・メールから 1〜2 文字のイニシャルを作る（アバターの画像 fallback 用）。
/// backend の /me は name/avatar を返さないため、email ローカル部や id から導出する。
export function initialsFrom(source: string | null | undefined): string {
  if (!source) return "?";
  const local = source.includes("@") ? source.slice(0, source.indexOf("@")) : source;
  const parts = local.split(/[.\-_\s]+/u).filter(Boolean);
  if (parts.length === 0) return local.slice(0, 1).toUpperCase() || "?";
  if (parts.length === 1) return parts[0].slice(0, 2).toUpperCase();
  return (parts[0][0] + parts[1][0]).toUpperCase();
}
