"use client";

/// glide-data-grid のテーマ解決（CsvGrid / CsvDraftGrid 共用・Task 11P.8 から抽出）。

import type { Theme } from "@glideapps/glide-data-grid";
import { useTheme } from "next-themes";
import * as React from "react";

/// ⚠️ glide-data-grid の内部カラーパーサは oklch()/oklab()/color-mix() を解釈できず黒に
/// フォールバックする（ダークでは黒がたまたま馴染み、ライトで破綻する）。canvas.fillStyle は
/// oklch を rgb へ正規化してくれない（そのまま保持する）ため、1px 実際に塗って getImageData で
/// sRGB の RGBA を読み戻す＝どの色空間の入力でも確実に "rgba(r,g,b,a)" へ変換する。
let _colorCtx: CanvasRenderingContext2D | null = null;
export function resolveColor(input: string): string {
  if (!input) return input;
  if (!_colorCtx) {
    const c = document.createElement("canvas");
    c.width = c.height = 1;
    _colorCtx = c.getContext("2d", { willReadFrequently: true });
  }
  if (!_colorCtx) return input;
  try {
    _colorCtx.clearRect(0, 0, 1, 1);
    _colorCtx.fillStyle = input;
    _colorCtx.fillRect(0, 0, 1, 1);
    const [r, g, b, a] = _colorCtx.getImageData(0, 0, 1, 1).data;
    return `rgba(${r}, ${g}, ${b}, ${(a / 255).toFixed(3)})`;
  } catch {
    return input;
  }
}

/// glide-data-grid の配色をアプリのセマンティックトークンへ揃える（ライト/ダーク対応）。
/// oklch トークンを getComputedStyle で読み → resolveColor で rgb/hex に正規化して渡す。
/// テーマ切替（next-themes）で再計算する。編集セルのハイライト色も同時に返す。
export function useGlideTheme(): { theme: Partial<Theme>; editedBg: string } {
  const { resolvedTheme } = useTheme();
  const [state, setState] = React.useState<{ theme: Partial<Theme>; editedBg: string }>({
    theme: {},
    editedBg: "rgba(0,0,0,0.05)",
  });

  React.useEffect(() => {
    // クラス反映後に読むため 1 フレーム遅らせる。
    const id = requestAnimationFrame(() => {
      const cs = getComputedStyle(document.documentElement);
      const v = (name: string) => cs.getPropertyValue(name).trim();
      const rc = (name: string) => resolveColor(v(name));
      setState({
        theme: {
          accentColor: rc("--primary"),
          accentLight: rc("--accent"),
          textDark: rc("--foreground"),
          textMedium: rc("--muted-foreground"),
          textLight: rc("--muted-foreground"),
          textBubble: rc("--foreground"),
          bgIconHeader: rc("--muted-foreground"),
          fgIconHeader: rc("--background"),
          textHeader: rc("--muted-foreground"),
          textHeaderSelected: rc("--foreground"),
          bgCell: rc("--card"),
          bgCellMedium: rc("--muted"),
          bgHeader: rc("--muted"),
          bgHeaderHasFocus: rc("--accent"),
          bgHeaderHovered: rc("--accent"),
          bgBubble: rc("--popover"),
          bgSearchResult: rc("--accent"),
          borderColor: rc("--border"),
          drilldownBorder: rc("--border"),
          linkColor: rc("--primary"),
          fontFamily: v("--font-sans") || "ui-sans-serif, system-ui, sans-serif",
          baseFontStyle: "13px",
          headerFontStyle: "600 12px",
        },
        editedBg: resolveColor(`color-mix(in oklab, ${v("--primary")} 12%, ${v("--card")})`),
      });
    });
    return () => cancelAnimationFrame(id);
  }, [resolvedTheme]);

  return state;
}
