// 地図のスタイル（提供元のスタイル JSON）を Shiki の配色に塗り直す。
//
// 配信元（OpenFreeMap・自前のタイル配信）ごとにスタイルを作り込まず、読み込んだスタイルの
// レイヤーを種類（塗り・線・文字）と出典のレイヤー名（water・park・road…）で見分けて色を差し替える。
// OpenMapTiles のレイヤー名を基準にし、似た名前の他のスキーマにも当たるようにしている。
// 色は zinc の地に、水は冬・緑地は夏の季節色を薄く敷き、店の POI と道路番号は消す（地点のピンを主役にする）。

import type { ExpressionSpecification, LayerSpecification, StyleSpecification } from "maplibre-gl";

export type MapMode = "light" | "dark";

type Palette = {
  land: string;
  water: string;
  waterLabel: string;
  green: string;
  landuse: string;
  building: string;
  road: string;
  roadMinor: string;
  casing: string;
  path: string;
  rail: string;
  border: string;
  label: string;
  labelStrong: string;
  halo: string;
};

/// globals.css の zinc・--season-winter・--season-summer を地図用に薄めた色。
/// MapLibre は oklch を読めないので 16 進で持つ。
export const MAP_PALETTE: Record<MapMode, Palette> = {
  light: {
    land: "#f3f3f1",
    water: "#cfdfe9",
    waterLabel: "#5f7f96",
    green: "#dde9d8",
    landuse: "#efeeec",
    building: "#e5e5e8",
    road: "#ffffff",
    roadMinor: "#fcfcfb",
    casing: "#e1e1e6",
    path: "#e2e2e6",
    rail: "#c4c4cc",
    border: "#b4b4c2",
    label: "#6b6b76",
    labelStrong: "#3a3a42",
    halo: "#f3f3f1",
  },
  dark: {
    land: "#1c1c20",
    water: "#18252f",
    waterLabel: "#6d8aa0",
    green: "#1d2a21",
    landuse: "#1f1f23",
    building: "#28282e",
    road: "#3a3a42",
    roadMinor: "#2f2f36",
    casing: "#1c1c20",
    path: "#2c2c33",
    rail: "#45454f",
    border: "#55556a",
    label: "#9a9aa6",
    labelStrong: "#dcdce0",
    halo: "#1c1c20",
  },
};

/// 道順の線（Yui の系列色 s1 = --chart-1）。
export const ROUTE_COLOR = "#398cb8";

const GREEN = /park|grass|wood|forest|garden|pitch|cemetery|golf|meadow|scrub|wetland|national_park|nature|landcover_(?!ice|glacier)/;
/// 消す文字: 道路番号・一方通行の矢印・番地・店などの POI・細い道の名前（地点の名前を主役にする）。
const HIDE_SYMBOL = /shield|one_way|oneway|road-number|road_number|housenum|poi_r\d|^poi-label$|^poi_label$|name-(minor|path)|label-(minor|path|small)/;
const TRANSIT = /transit|station|rail|subway/;

function sourceLayer(l: LayerSpecification): string {
  return "source-layer" in l && typeof l["source-layer"] === "string" ? l["source-layer"] : "";
}

/// 日本語の名前を優先する（OpenMapTiles は `name:ja`。`name_ja` の形のスキーマもある）。
/// 既定のスタイルは英語名か「英語\n日本語」の 2 段で出すため、利用者の言語に合わせて 1 段にする。
const NAME_JA: ExpressionSpecification = ["coalesce", ["get", "name:ja"], ["get", "name_ja"], ["get", "name"]];

function usesName(field: unknown): boolean {
  const s = JSON.stringify(field ?? "");
  return /name/.test(s) && !/"ref"|\{ref\}/.test(s);
}

function setPaint(l: LayerSpecification, key: string, value: unknown) {
  const paint = ((l as { paint?: Record<string, unknown> }).paint ??= {});
  paint[key] = value;
}

function hide(l: LayerSpecification) {
  const layout = ((l as { layout?: Record<string, unknown> }).layout ??= {});
  layout.visibility = "none";
}

function tintLayer(l: LayerSpecification, p: Palette): void {
  const id = l.id.toLowerCase();
  const src = sourceLayer(l).toLowerCase();
  const key = `${src} ${id}`;
  switch (l.type) {
    case "background":
      setPaint(l, "background-color", p.land);
      return;
    case "raster":
      // 低いズームの陰影起伏（Natural Earth）は色が強いので灰色にして薄く敷く。
      setPaint(l, "raster-saturation", -1);
      setPaint(l, "raster-opacity", 0.18);
      return;
    case "hillshade":
      hide(l);
      return;
    case "fill":
    case "fill-extrusion": {
      const prop = l.type === "fill" ? "fill-color" : "fill-extrusion-color";
      if (/water/.test(key)) setPaint(l, prop, p.water);
      else if (/building/.test(key)) {
        setPaint(l, prop, p.building);
        if (l.type === "fill") setPaint(l, "fill-opacity", 0.7);
      }
      else if (GREEN.test(key)) setPaint(l, prop, p.green);
      else if (/ice|glacier/.test(key)) setPaint(l, prop, p.land);
      else if (/road|transportation|aeroway|bridge|tunnel|pier/.test(key)) setPaint(l, prop, p.roadMinor);
      else setPaint(l, prop, p.landuse);
      if (l.type === "fill") {
        // 塗りの縁取り（fill-outline-color）が元の色のまま残ると、緑地や建物に濃い輪郭が出る。
        const paint = (l as { paint?: Record<string, unknown> }).paint;
        if (paint && "fill-outline-color" in paint) delete paint["fill-outline-color"];
        if (paint && "fill-pattern" in paint) delete paint["fill-pattern"];
      }
      return;
    }
    case "line":
      if (/water/.test(key)) setPaint(l, "line-color", p.water);
      else if (/boundary|admin/.test(key)) setPaint(l, "line-color", p.border);
      else if (/rail|transit/.test(key)) setPaint(l, "line-color", p.rail);
      else if (/casing|case/.test(id)) setPaint(l, "line-color", p.casing);
      else if (/path|footway|pedestrian|steps|track|cycle/.test(key)) setPaint(l, "line-color", p.path);
      else if (/motorway|trunk|primary|major/.test(id)) setPaint(l, "line-color", p.road);
      else if (/road|transportation|highway|street|bridge|tunnel|aeroway/.test(key)) setPaint(l, "line-color", p.roadMinor);
      else setPaint(l, "line-color", p.border);
      if ("paint" in l && l.paint && "line-pattern" in l.paint) delete (l.paint as Record<string, unknown>)["line-pattern"];
      return;
    case "symbol": {
      const isPoi = /poi/.test(key) && !TRANSIT.test(key);
      if (HIDE_SYMBOL.test(id) || isPoi) {
        hide(l);
        return;
      }
      const layout = (l.layout ??= {});
      if (usesName(layout["text-field"])) layout["text-field"] = NAME_JA;
      // 地図の文字はすべて控えめにする（濃い文字は地点の名前だけ）。
      setPaint(l, "text-color", /water|waterway|marine/.test(key) ? p.waterLabel : p.label);
      setPaint(l, "text-halo-color", p.halo);
      setPaint(l, "text-halo-width", 1.4);
      if (TRANSIT.test(key)) {
        // 駅の記号は灰色の地に馴染むよう薄くする（色そのものはアイコン画像なので変えられない）。
        setPaint(l, "icon-opacity", 0.75);
      }
      return;
    }
    default:
      return;
  }
}

/// スタイル JSON を Shiki の配色に塗り直した写しを返す（元は変えない）。
export function tintStyle(style: StyleSpecification, mode: MapMode): StyleSpecification {
  const p = MAP_PALETTE[mode];
  const next = structuredClone(style);
  for (const l of next.layers) tintLayer(l, p);
  return next;
}
