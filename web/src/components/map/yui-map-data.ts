// Yui の map（地点・道順）を MapLibre に渡す形へ写す（描画に依存しない純粋な関数）。
import type { MapFigure } from "@imanect/yui";

type Position = [number, number];

export type PointFeature = {
  type: "Feature";
  id: number;
  geometry: { type: "Point"; coordinates: Position };
  properties: { n: number; label: string; desc: string };
};

/// 地点（番号は Yui の図・一覧と同じ 1 始まり）。
export function markerFeatures(figure: MapFigure): { type: "FeatureCollection"; features: PointFeature[] } {
  return {
    type: "FeatureCollection",
    features: figure.markers.map((m, i) => ({
      type: "Feature",
      id: i,
      geometry: { type: "Point", coordinates: [m.lng, m.lat] },
      properties: { n: i + 1, label: m.label, desc: m.desc ?? "" },
    })),
  };
}

/// 道順（Yui と同じく、地点を順に直線で結ぶ。名前の分からない地点は飛ばす）。
export function routeFeature(figure: MapFigure): {
  type: "Feature";
  geometry: { type: "LineString"; coordinates: Position[] };
  properties: { mode: string };
} | null {
  if (!figure.route) return null;
  const coords: Position[] = [];
  for (const stop of figure.route.stops) {
    const m = figure.markers.find((mk) => mk.label === stop);
    if (m) coords.push([m.lng, m.lat]);
  }
  if (coords.length < 2) return null;
  return {
    type: "Feature",
    geometry: { type: "LineString", coordinates: coords },
    properties: { mode: figure.route.mode ?? "" },
  };
}

/// 最初に見せる範囲。地点が 1 つ（または同じ場所）なら中心とズーム、複数なら全地点を囲む範囲。
export type InitialView =
  | { kind: "center"; center: Position; zoom: number }
  | { kind: "bounds"; bounds: [Position, Position]; maxZoom: number };

export function initialView(figure: MapFigure): InitialView {
  const lngs = figure.markers.map((m) => m.lng);
  const lats = figure.markers.map((m) => m.lat);
  const sw: Position = [Math.min(...lngs), Math.min(...lats)];
  const ne: Position = [Math.max(...lngs), Math.max(...lats)];
  // Yui の zoom（1〜20）は寄りすぎ防止の上限として使う。
  const zoom = Math.min(Math.max(figure.zoom ?? 15, 1), 18);
  if (ne[0] - sw[0] < 1e-6 && ne[1] - sw[1] < 1e-6) {
    return { kind: "center", center: sw, zoom };
  }
  return { kind: "bounds", bounds: [sw, ne], maxZoom: Math.min(figure.zoom ?? 16, 17) };
}

/// 道路沿いの道順を問い合わせる内容。徒歩（walk）と車（drive）だけを道路に沿わせる。
/// 公共交通（transit）と移動手段の指定が無い道順は、実際の経路と誤解させないよう直線のままにする。
export function routeQuery(
  figure: MapFigure,
): { mode: "walk" | "drive"; stops: { lat: number; lng: number }[] } | null {
  const mode = figure.route?.mode;
  if (mode !== "walk" && mode !== "drive") return null;
  const stops: { lat: number; lng: number }[] = [];
  for (const stop of figure.route?.stops ?? []) {
    const m = figure.markers.find((mk) => mk.label === stop);
    if (m) stops.push({ lat: m.lat, lng: m.lng });
  }
  return stops.length >= 2 ? { mode, stops } : null;
}

/// 距離と時間の短い表記（例「徒歩 1.2 km・約 15 分」）。
export function routeSummary(mode: "walk" | "drive", distanceM: number, durationS: number): string {
  // 10 m 単位に丸めて 1000 m になるもの（995 m 以上）は km で書く。
  const km = distanceM >= 995 ? `${(distanceM / 1000).toFixed(1)} km` : `${Math.round(distanceM / 10) * 10} m`;
  const min = Math.max(1, Math.round(durationS / 60));
  const time = min >= 60 ? `約 ${Math.floor(min / 60)} 時間 ${min % 60} 分` : `約 ${min} 分`;
  return `${mode === "walk" ? "徒歩" : "車"} ${km}・${time}`;
}
