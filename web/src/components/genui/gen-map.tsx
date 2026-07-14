"use client";

/// generative UI の地図（MapLibre GL・PR5）。
///
/// AI は座標・マーカー・ルート waypoint など**構造化データのみ**を渡す（サーバ検証済み）。
/// タイル/スタイルの URL は AI ではなく**サーバ設定**（`NEXT_PUBLIC_MAP_STYLE_URL`）で注入する
/// （信頼境界）。既定は key 不要の実タイル（CARTO/OSM・ライト/ダーク切替）で本物の街路地図を描き、
/// 運用側で自己ホスト/商用スタイル/オフラインタイルに差し替えられる。タイル取得に失敗する環境
/// （air-gapped で未設定 等）では地点一覧へ安全に縮退する。route/marker のアクセント色は season
/// トークンを実色へ解決して描く。重量級の maplibre-gl は spec-renderer が `next/dynamic`
/// （ssr:false）で map ノードがある時だけ遅延ロードする。

import * as React from "react";

import maplibregl from "maplibre-gl";
import type { Feature, LineString } from "geojson";
import { MapPin } from "lucide-react";

import "maplibre-gl/dist/maplibre-gl.css";

import type { MapProps, MarkerKind, RouteMode } from "@/generated/gui-spec";
import { currentSeasonIndex, seasonAccentStyle } from "@/lib/season";

/// マーカー種別 → 色（CSS 変数式）。season トークンと semantic を織り交ぜる。
const MARKER_COLOR: Record<MarkerKind, string> = {
  place: "var(--primary)",
  start: "var(--season-summer)",
  end: "var(--season-autumn)",
  stop: "var(--muted-foreground)",
  lodging: "var(--season-winter)",
  food: "var(--season-autumn)",
  sight: "var(--season)",
};

/// 徒歩/公共交通は破線、車/飛行機は実線（移動手段を線種で示す）。
const DASHED_MODES: RouteMode[] = ["walking", "transit"];

/// CSS 変数式（var(--season) 等）を実際の色（#rrggbb / rgba）へ解決する。
/// getComputedStyle で使用値を得たあと 1px 塗って getImageData で sRGB バイトを読む
/// （トークンは oklch なので canvas.fillStyle の文字列正規化では oklch のまま返り、MapLibre の
/// style-spec が解釈できない。ピクセルを読めば色空間に依らず必ず sRGB になる）。
function makeColorResolver(host: HTMLElement) {
  const probe = document.createElement("span");
  probe.style.display = "none";
  host.appendChild(probe);
  const canvas = document.createElement("canvas");
  canvas.width = 1;
  canvas.height = 1;
  const ctx = canvas.getContext("2d", { willReadFrequently: true });
  const hex2 = (n: number) => n.toString(16).padStart(2, "0");
  return {
    resolve(expr: string, fallback = "#888888"): string {
      try {
        probe.style.color = "";
        probe.style.color = expr;
        const used = getComputedStyle(probe).color;
        if (!ctx || !used) return fallback;
        ctx.clearRect(0, 0, 1, 1);
        ctx.fillStyle = used;
        ctx.fillRect(0, 0, 1, 1);
        const [r, g, b, a] = ctx.getImageData(0, 0, 1, 1).data;
        return a === 255
          ? `#${hex2(r)}${hex2(g)}${hex2(b)}`
          : `rgba(${r},${g},${b},${(a / 255).toFixed(3)})`;
      } catch {
        return fallback;
      }
    },
    dispose() {
      host.removeChild(probe);
    },
  };
}

/// ルート waypoint の経度を連続化する（隣接点の差が 180°を超えたら ±360 して短い方を採る）。
/// 日付変更線を跨ぐルートが世界の反対側を回る長い線として描かれるのを防ぐ。
function unwrapRoute(wps: { lat: number; lng: number }[]): [number, number][] {
  const out: [number, number][] = [];
  let prev = 0;
  wps.forEach((w, i) => {
    let lng = w.lng;
    if (i > 0) {
      while (lng - prev > 180) lng -= 360;
      while (lng - prev < -180) lng += 360;
    }
    prev = lng;
    out.push([lng, w.lat]);
  });
  return out;
}

/// 移動手段 → OSRM プロファイル（道なりに追従できるもののみ）。transit/flight は経路網が
/// 無い/意味が違うため直線のまま描く。
const ROUTE_PROFILE: Record<RouteMode, string | null> = {
  walking: "foot",
  driving: "car",
  transit: null,
  flight: null,
};

/// 経由地を道なりの経路へスナップする（キー不要の OSRM・既定は FOSSGIS の公開ルータ）。
/// `NEXT_PUBLIC_MAP_ROUTING_URL` で自己ホスト/商用ルータへ差し替え可能（設定式・信頼境界）。
/// 失敗（air-gapped/CI/レート制限）時は null を返し、呼び出し側は直線にフォールバックする。
async function snapRoute(
  waypoints: { lat: number; lng: number }[],
  mode: RouteMode,
): Promise<[number, number][] | null> {
  const profile = ROUTE_PROFILE[mode];
  if (!profile || waypoints.length < 2) return null;
  const base = process.env.NEXT_PUBLIC_MAP_ROUTING_URL ?? "https://routing.openstreetmap.de";
  const coords = waypoints.map((w) => `${w.lng},${w.lat}`).join(";");
  const url = `${base}/routed-${profile}/route/v1/${profile}/${coords}?overview=full&geometries=geojson`;
  try {
    const res = await fetch(url);
    if (!res.ok) return null;
    const json: unknown = await res.json();
    const geom = (json as { routes?: { geometry?: { coordinates?: [number, number][] } }[] })
      ?.routes?.[0]?.geometry?.coordinates;
    return Array.isArray(geom) && geom.length >= 2 ? geom : null;
  } catch {
    return null;
  }
}

/// data の全座標を含む LngLatBounds を作る（fitBounds 用）。
function collectBounds(map: MapProps): maplibregl.LngLatBounds | null {
  const pts: [number, number][] = [];
  for (const m of map.markers ?? []) pts.push([m.lng, m.lat]);
  for (const w of map.route?.waypoints ?? []) pts.push([w.lng, w.lat]);
  if (pts.length === 0) return null;
  const b = new maplibregl.LngLatBounds(pts[0], pts[0]);
  for (const p of pts) b.extend(p);
  return b;
}

/// 既定ベースマップ（key 不要の CARTO ラスタ・OSM ベース）。ライト/ダークで実タイルを切替。
/// 商用/自己ホスト/オフラインは NEXT_PUBLIC_MAP_STYLE_URL で差し替える（設定式タイル）。
function basemapStyle(dark: boolean): maplibregl.StyleSpecification {
  const variant = dark ? "dark_all" : "light_all";
  return {
    version: 8,
    sources: {
      base: {
        type: "raster",
        tiles: [`https://a.basemaps.cartocdn.com/${variant}/{z}/{x}/{y}.png`],
        tileSize: 256,
        maxzoom: 20,
        attribution: '© <a href="https://www.openstreetmap.org/copyright">OpenStreetMap</a> © CARTO',
      },
    },
    layers: [{ id: "base", type: "raster", source: "base" }],
  };
}

/// prefers-color-scheme・`.dark` クラス・data-theme のいずれの切替でも再描画するためのティック。
/// 色トークンは mount 時に実色へ焼くため、テーマが変わったら地図を作り直す必要がある。
function useThemeTick(): number {
  const [tick, setTick] = React.useState(0);
  React.useEffect(() => {
    const bump = () => setTick((t) => t + 1);
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    mq.addEventListener("change", bump);
    const mo = new MutationObserver(bump);
    mo.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["class", "style", "data-theme"],
    });
    return () => {
      mq.removeEventListener("change", bump);
      mo.disconnect();
    };
  }, []);
  return tick;
}

/// 番号付きピン（ルート順）または種別ドットの DOM 要素を作る（GL の symbol 層は使わず、
/// テキストのグリフ依存を避ける）。バッジ＋ラベルを縦フローに積み、anchor:bottom で座標に
/// 合わせる（絶対配置は maplibre の anchor 計算幅を狂わせるため使わない）。白リング・淡い影で
/// node-card 風の質感にする。
function buildPinElement(
  color: string,
  order: number | null,
  label: string | null,
  clickable: boolean,
): HTMLElement {
  const el = document.createElement("div");
  el.className = "genui-map-marker";
  // 既定は地図ドラッグを妨げないよう pointer-events を切るが、説明ポップアップを持つ
  // マーカーはドット部分だけクリック可能にする。
  el.style.cssText =
    "display:flex;flex-direction:column;align-items:center;gap:3px;pointer-events:none;";

  const dot = document.createElement("div");
  const size = order !== null ? 24 : 15;
  dot.style.cssText = [
    `width:${size}px`,
    `height:${size}px`,
    "border-radius:9999px",
    `background:${color}`,
    "border:2px solid var(--map-ring)",
    "box-shadow:0 1px 3px rgb(0 0 0 / 0.3)",
    "display:flex;align-items:center;justify-content:center",
    "color:#fff;font-size:12px;font-weight:700;line-height:1;font-variant-numeric:tabular-nums",
    ...(clickable ? ["pointer-events:auto", "cursor:pointer"] : []),
  ].join(";");
  if (order !== null) dot.textContent = String(order);
  el.appendChild(dot);

  if (label) {
    const chip = document.createElement("div");
    chip.textContent = label;
    chip.style.cssText = [
      "max-width:132px",
      "padding:1px 7px",
      "border-radius:9999px",
      "background:var(--map-chip-bg)",
      "color:var(--map-chip-fg)",
      "border:1px solid var(--map-chip-border)",
      "font-size:11px;font-weight:600;line-height:1.5",
      "white-space:nowrap;overflow:hidden;text-overflow:ellipsis",
      "box-shadow:0 1px 2px rgb(0 0 0 / 0.14)",
    ].join(";");
    el.appendChild(chip);
  }
  return el;
}

export function GenUiMap({ map }: { map: MapProps }) {
  const containerRef = React.useRef<HTMLDivElement>(null);
  const [failed, setFailed] = React.useState(false);
  const seasonIdx = currentSeasonIndex();
  const themeTick = useThemeTick();

  React.useEffect(() => {
    const host = containerRef.current;
    if (!host) return;
    const styleUrl = process.env.NEXT_PUBLIC_MAP_STYLE_URL;
    const resolver = makeColorResolver(host);
    let instance: maplibregl.Map | null = null;
    try {
      // 実色の解決（route/marker のアクセント。地図タイルは実タイルなので背景色は不要）。
      const routeColor = resolver.resolve("var(--season)", "#3b82f6");
      const casing = resolver.resolve("var(--card)", "#ffffff");
      // マーカー DOM 要素が参照する CSS 変数を host に注入（実色）。
      host.style.setProperty("--map-ring", resolver.resolve("var(--card)", "#ffffff"));
      host.style.setProperty("--map-chip-bg", resolver.resolve("var(--card)", "#ffffff"));
      host.style.setProperty("--map-chip-fg", resolver.resolve("var(--foreground)", "#111111"));
      host.style.setProperty("--map-chip-border", resolver.resolve("var(--border)", "#e5e5ea"));

      const dataBounds = collectBounds(map);
      // 既定は key 不要の実タイル（CARTO ラスタ・OSM ベース）で本物の街路地図を描く。
      // ライト/ダークでベースマップを切り替える（themeTick で作り直す）。運用では
      // NEXT_PUBLIC_MAP_STYLE_URL で自己ホスト/商用スタイルに差し替える（設定式タイル・信頼境界）。
      const isDark =
        document.documentElement.classList.contains("dark") ||
        window.matchMedia("(prefers-color-scheme: dark)").matches;
      const style: maplibregl.StyleSpecification | string = styleUrl ?? basemapStyle(isDark);

      instance = new maplibregl.Map({
        container: host,
        style,
        center: [map.center.lng, map.center.lat],
        zoom: map.zoom ?? 11,
        attributionControl: { compact: true },
        scrollZoom: false, // チャット内でのスクロール横取りを防ぐ（+/- は NavigationControl）。
        dragRotate: false,
        pitchWithRotate: false,
      });
      // 個々のタイル取得失敗（source エラー）は部分描画で継続してよい。スタイル解釈失敗や
      // WebGL コンテキストロスト等の致命的失敗のみ一覧へ縮退する（try/catch では届かない）。
      instance.on("error", (e) => {
        if ((e as { sourceId?: string })?.sourceId) return;
        setFailed(true);
      });
      instance.addControl(
        new maplibregl.NavigationControl({ showCompass: false, visualizePitch: false }),
        "top-right",
      );

      const m = instance;
      m.on("load", () => {
        // ルート（順序付き waypoint を線で結ぶ・casing で下地を敷いてコントラストを上げる）。
        const wps = map.route?.waypoints ?? [];
        if (map.route && wps.length >= 2) {
          const routeMode = map.route.mode;
          const line = (coords: [number, number][]): Feature<LineString> => ({
            type: "Feature",
            properties: {},
            geometry: { type: "LineString", coordinates: coords },
          });
          // まず直線（日付変更線跨ぎは経度連続化）で即描画し、道なり経路が取れたら差し替える。
          m.addSource("route", { type: "geojson", data: line(unwrapRoute(wps)) });
          m.addLayer({
            id: "route-casing",
            type: "line",
            source: "route",
            layout: { "line-cap": "round", "line-join": "round" },
            paint: { "line-color": casing, "line-width": 7, "line-opacity": 0.9 },
          });
          m.addLayer({
            id: "route",
            type: "line",
            source: "route",
            layout: { "line-cap": "round", "line-join": "round" },
            paint: {
              "line-color": routeColor,
              "line-width": 3.5,
              ...(DASHED_MODES.includes(routeMode) ? { "line-dasharray": [1.5, 1.2] } : {}),
            },
          });
          // 徒歩/車は道なりにスナップ（キー不要ルータ・失敗時は直線のまま）。
          void snapRoute(wps, routeMode).then((snapped) => {
            const src = snapped && m.getSource("route");
            if (src) (src as maplibregl.GeoJSONSource).setData(line(snapped));
          });
        }

        // マーカー（ルートがあれば順序番号、無ければ種別ドット）。
        const numbered = (map.markers?.length ?? 0) > 0 && wps.length >= 2;
        (map.markers ?? []).forEach((mk, i) => {
          const color = resolver.resolve(MARKER_COLOR[mk.kind] ?? MARKER_COLOR.place);
          const hasDetail = Boolean(mk.description);
          const el = buildPinElement(color, numbered ? i + 1 : null, mk.label ?? null, hasDetail);
          const marker = new maplibregl.Marker({ element: el, anchor: "center" }).setLngLat([
            mk.lng,
            mk.lat,
          ]);
          // 説明はラベルとは別に保持し、クリックでポップアップ表示する（詳細を失わない）。
          if (mk.description) {
            marker.setPopup(
              new maplibregl.Popup({ offset: 16, closeButton: false }).setText(
                [mk.label, mk.description].filter(Boolean).join(" — "),
              ),
            );
          }
          marker.addTo(m);
        });

        // 表示範囲: 明示 bounds > データ包含 > center+zoom。
        if (map.bounds) {
          m.fitBounds(
            [
              [map.bounds.west, map.bounds.south],
              [map.bounds.east, map.bounds.north],
            ],
            { padding: 40, duration: 0 },
          );
        } else if (dataBounds) {
          m.fitBounds(dataBounds, { padding: 64, maxZoom: 16, duration: 0 });
        }
      });
    } catch {
      setFailed(true);
    }

    return () => {
      instance?.remove();
      resolver.dispose();
    };
  }, [map, seasonIdx, themeTick]);

  // 縮退一覧は未検証 payload（note 埋め込み等）でも壊れないよう配列を保証する。
  const markers = Array.isArray(map.markers) ? map.markers : [];

  return (
    <figure
      data-testid="genui-map"
      style={seasonAccentStyle(seasonIdx)}
      className="min-w-0"
    >
      {map.title ? (
        <figcaption className="mb-2 flex items-center gap-1.5 text-sm font-semibold tracking-tight text-foreground">
          <MapPin className="size-4 text-[var(--season)]" aria-hidden />
          {map.title}
        </figcaption>
      ) : null}
      {failed ? (
        // 縮退: 地図が描けない環境でも行程情報は失わない（マーカー一覧で提示）。
        <ul className="rounded-xl border border-border/60 bg-card/40 p-3 text-[13px]">
          {markers.map((mk, i) => (
            <li key={i} className="flex items-baseline gap-2 py-0.5">
              <span className="font-medium text-foreground">{mk.label ?? `地点 ${i + 1}`}</span>
              {mk.description ? (
                <span className="text-muted-foreground">{mk.description}</span>
              ) : null}
            </li>
          ))}
        </ul>
      ) : (
        <div
          ref={containerRef}
          data-testid="genui-map-canvas"
          className="h-72 w-full overflow-hidden rounded-xl border border-border/60 shadow-sm"
        />
      )}
    </figure>
  );
}
