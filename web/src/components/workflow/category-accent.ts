/// ノードのカテゴリを四季アクセントへ写像する（storage/ai/control… を色で識別する）。
/// 12 カテゴリを 4 季にまとめる: 生成/創出=春、連携/通知=夏、制御/変換=秋、データ/記憶=冬。
/// 色は seasonVar 経由でトークンに解決する（彩度は token 側で抑制済み）。

import { seasonVar } from "@/lib/season";

const CATEGORY_SEASON: Record<string, number> = {
  ai: 0,
  skill: 0,
  external: 1,
  developer: 1,
  notify: 1,
  office: 1,
  control: 2,
  workflow: 2,
  transform: 2,
  storage: 3,
  memory: 3,
  data: 3,
};

/// カテゴリ → 季節インデックス（未知は秋=2 に寄せる）。
export function categorySeasonIndex(category: string | undefined): number {
  return category != null && category in CATEGORY_SEASON ? CATEGORY_SEASON[category] : 2;
}

/// カテゴリ → 季節色の CSS 変数文字列。
export function categoryVar(category: string | undefined): string {
  return seasonVar(categorySeasonIndex(category));
}
