// 提供元のスタイルを、MapLibre が読める範囲に削る。
//
// Mapbox のスタイル（v11 以降）は MapLibre の仕様に無い項目（`fog`・`projection.name`・`slot`・
// `*-emissive-strength`・`model` レイヤーなど）を持つ。MapLibre はスタイルの検証で 1 つでも誤りがあると
// スタイル全体を読まない（地図が出ない）ので、仕様に無い項目を落とし、なお検証を通らないレイヤーは外す。
// 地図を出すときにだけ読む（use-yui-map.ts から動的 import）。

import { latest, validateStyleMin } from "@maplibre/maplibre-gl-style-spec";
import type { StyleSpecification } from "maplibre-gl";

type Spec = Record<string, Record<string, unknown> | undefined>;

/// 検証で落ちても外さない最上位の項目（無いとスタイルとして成り立たない）。
const REQUIRED = new Set(["version", "sources", "layers"]);

function pick(obj: unknown, allowed: Record<string, unknown> | undefined): Record<string, unknown> | undefined {
  if (!obj || typeof obj !== "object") return undefined;
  return Object.fromEntries(Object.entries(obj).filter(([k]) => !!allowed && k in allowed));
}

export function sanitizeStyle(input: StyleSpecification): StyleSpecification {
  const spec = latest as unknown as Spec;
  const root = spec.$root ?? {};
  const layerKeys = spec.layer ?? {};
  const layerTypes = ((layerKeys.type as { values?: Record<string, unknown> } | undefined)?.values ?? {}) as Record<string, unknown>;

  const style = Object.fromEntries(
    Object.entries(structuredClone(input) as unknown as Record<string, unknown>).filter(([k]) => k in root),
  ) as Record<string, unknown>;
  const layers = (Array.isArray(style.layers) ? style.layers : []) as Record<string, unknown>[];
  style.layers = layers
    .filter((l) => typeof l.type === "string" && l.type in layerTypes)
    .map((l) => {
      const out = Object.fromEntries(Object.entries(l).filter(([k]) => k in layerKeys));
      const paint = pick(l.paint, spec[`paint_${String(l.type)}`]);
      const layout = pick(l.layout, spec[`layout_${String(l.type)}`]);
      if (paint) out.paint = paint;
      else delete out.paint;
      if (layout) out.layout = layout;
      else delete out.layout;
      return out;
    });

  // 最上位の項目は 1 つずつ検証して、通らないものを外す（Mapbox の `projection: { name }` などは、
  // 検証のメッセージにどの項目の誤りかが出ない）。
  for (const key of Object.keys(style)) {
    if (REQUIRED.has(key) || key === "sources") continue;
    const probe = { version: 8, sources: {}, layers: [], [key]: style[key] } as unknown as StyleSpecification;
    if (validateStyleMin(probe).length > 0) delete style[key];
  }

  // 値の誤り（MapLibre に無い式の演算子など）は項目名では見分けられないので、検証の結果で外す。
  for (let round = 0; round < 3; round++) {
    const errors = validateStyleMin(style as unknown as StyleSpecification);
    if (errors.length === 0) break;
    const dropLayers = new Set<number>();
    let changed = false;
    for (const e of errors) {
      const layer = /^layers\[(\d+)\]/.exec(e.message);
      if (layer) {
        dropLayers.add(Number(layer[1]));
        continue;
      }
      const key = /^([A-Za-z-]+)/.exec(e.message)?.[1];
      if (key && !REQUIRED.has(key) && key in style) {
        delete style[key];
        changed = true;
      }
    }
    if (dropLayers.size === 0 && !changed) break;
    style.layers = (style.layers as unknown[]).filter((_, i) => !dropLayers.has(i));
  }
  return style as unknown as StyleSpecification;
}
