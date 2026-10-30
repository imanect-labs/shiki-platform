// 回答の ```ui の map（Yui の MapFigure）を MapLibre で描くフック。
//
// - 地図の部品（maplibre-gl・約 800KB）は、地図が画面に近づいたときに初めて読む。
// - WebGL の文脈はブラウザごとに十数個までしか持てない。長い会話に地図がいくつあっても壊れないよう、
//   画面から離れた地図は破棄し、戻ってきたら作り直す（`active`）。
// - スタイルは提供元のものを読み、Shiki の配色に塗り直してから渡す（map-style.ts）。
// - 生成中は地点が 1 行ずつ増える。地図は作り直さず、ピン・道順・視点だけを更新する。
"use client";

import * as React from "react";
import type {
  FilterSpecification,
  GeoJSONSource,
  LngLatBoundsLike,
  Map as MaplibreMap,
  Marker,
  StyleSpecification,
} from "maplibre-gl";
import type { MapFigure } from "@imanect/yui";

import type { MapConfig } from "@/lib/map-api";
import { mapboxTransform, mapboxUrl, MAP_PALETTE, ROUTE_COLOR, tintStyle, type MapMode } from "./map-style";

type Maplibre = typeof import("maplibre-gl");

let lib: Promise<Maplibre> | null = null;
function loadMaplibre(): Promise<Maplibre> {
  lib ??= Promise.all([import("maplibre-gl"), import("maplibre-gl/dist/maplibre-gl.css")])
    .then(([m]) => ((m as unknown as { default?: Maplibre }).default ?? m) as Maplibre)
    .catch((e: unknown) => {
      lib = null;
      throw e;
    });
  return lib;
}

const styles = new Map<string, Promise<StyleSpecification>>();
/// スタイル JSON は地図ごとに読み直さない（同じ URL は 1 回）。
function loadStyle(url: string, token: string | null | undefined): Promise<StyleSpecification> {
  let p = styles.get(url);
  if (!p) {
    p = fetch(mapboxUrl(url, token))
      .then(async (r) => {
        if (!r.ok) throw new Error(`style ${r.status}`);
        return (await r.json()) as StyleSpecification;
      })
      .catch((e: unknown) => {
        styles.delete(url);
        throw e;
      });
    styles.set(url, p);
  }
  return p;
}

const ROUTE = "yui-route";
const PLACES = "yui-places";
/// 地点が多いと名前が重なるので、それ以上は選んだ地点の名前だけを出す。
const MAX_LABELS = 8;

function routeData(figure: MapFigure): GeoJSON.FeatureCollection {
  const at = new Map(figure.markers.map((m) => [m.label, [m.lng, m.lat] as [number, number]]));
  const coords = (figure.route?.stops ?? []).map((s) => at.get(s)).filter((c): c is [number, number] => !!c);
  return {
    type: "FeatureCollection",
    features: coords.length >= 2 ? [{ type: "Feature", properties: {}, geometry: { type: "LineString", coordinates: coords } }] : [],
  };
}

function placesData(figure: MapFigure): GeoJSON.FeatureCollection {
  const always = figure.markers.length <= MAX_LABELS;
  return {
    type: "FeatureCollection",
    features: figure.markers.map((m, i) => ({
      type: "Feature",
      properties: { i, label: m.label, always },
      geometry: { type: "Point", coordinates: [m.lng, m.lat] },
    })),
  };
}

function labelFilter(selected: number | null): FilterSpecification {
  return ["any", ["==", ["get", "always"], true], ["==", ["get", "i"], selected ?? -1]];
}

/// 地点の名前に使う書体。提供元のスタイルが持つ書体（グリフ）からしか選べないので、
/// 既存の文字のレイヤーから太字（無ければ中字・どれか）を拾う。
function labelFont(style: StyleSpecification): string[] {
  const fonts = style.layers
    .map((l) => (l.type === "symbol" ? l.layout?.["text-font"] : undefined))
    .filter((f): f is string[] => Array.isArray(f) && f.every((x) => typeof x === "string"));
  return (
    fonts.find((f) => f.some((x) => /bold/i.test(x))) ??
    fonts.find((f) => f.some((x) => /medium/i.test(x))) ??
    fonts[0] ?? ["Noto Sans Regular"]
  );
}

/// 塗り直したスタイルに、道順の線と地点の名前を足す。
/// 名前は地図の文字と同じ仕組みで描き、ぶつかる地図側の地名・道路名を消させる（DOM で重ねると両方が読めなくなる）。
/// ピン（番号の丸）は押せる部品なので DOM のまま置く（スタイルを差し替えても残る）。
function withOverlay(base: StyleSpecification, mode: MapMode, figure: MapFigure, selected: number | null): StyleSpecification {
  const s = tintStyle(base, mode);
  const p = MAP_PALETTE[mode];
  s.sources[ROUTE] = { type: "geojson", data: routeData(figure) };
  s.sources[PLACES] = { type: "geojson", data: placesData(figure) };
  s.layers.push(
    {
      id: `${ROUTE}-halo`,
      type: "line",
      source: ROUTE,
      layout: { "line-cap": "round", "line-join": "round" },
      paint: { "line-color": p.halo, "line-width": 7, "line-opacity": 0.85 },
    },
    {
      id: ROUTE,
      type: "line",
      source: ROUTE,
      layout: { "line-join": "round" },
      paint: { "line-color": ROUTE_COLOR, "line-width": 3, "line-dasharray": [2, 1.4] },
    },
    {
      id: `${PLACES}-label`,
      type: "symbol",
      source: PLACES,
      filter: labelFilter(selected),
      layout: {
        "text-field": ["get", "label"],
        "text-font": labelFont(s),
        "text-size": 12.5,
        "text-anchor": "top",
        "text-offset": [0, 1.3],
        "text-max-width": 9,
        "text-allow-overlap": true,
        "symbol-sort-key": ["get", "i"],
      },
      paint: { "text-color": p.labelStrong, "text-halo-color": p.halo, "text-halo-width": 1.8 },
    },
  );
  return s;
}

function bounds(figure: MapFigure): LngLatBoundsLike | null {
  if (figure.markers.length === 0) return null;
  const lngs = figure.markers.map((m) => m.lng);
  const lats = figure.markers.map((m) => m.lat);
  return [
    [Math.min(...lngs), Math.min(...lats)],
    [Math.max(...lngs), Math.max(...lats)],
  ];
}

/// 右は操作ボタン、下は名前と出典のぶん広く空ける。
const PADDING = { top: 48, bottom: 64, left: 48, right: 64 };
export type MapPadding = Partial<typeof PADDING>;

/// 地点が 1 つならその点に、複数なら全体が収まるように寄せる。`zoom:` があればその縮尺。
function frame(map: MaplibreMap, figure: MapFigure, animate: boolean, padding: MapPadding = {}) {
  const b = bounds(figure);
  if (!b) return;
  const [[w, s], [e, n]] = b as [[number, number], [number, number]];
  const center: [number, number] = [(w + e) / 2, (s + n) / 2];
  if (figure.markers.length === 1 || figure.zoom !== undefined) {
    map.jumpTo({ center, zoom: figure.zoom ?? 15 });
    return;
  }
  map.fitBounds(b, { padding: { ...PADDING, ...padding }, maxZoom: 16, animate, duration: animate ? 400 : 0 });
}

/// ピン: 番号の丸（primary の紺に白の縁）。名前は地図の文字のレイヤーで描く（withOverlay）。
function pinElement(index: number, label: string, onSelect: (i: number) => void): HTMLElement {
  const button = document.createElement("button");
  button.type = "button";
  button.dataset.testid = "yui-map-pin";
  button.setAttribute("aria-label", `${index + 1}. ${label}`);
  button.className =
    "flex size-7 items-center justify-center rounded-full border-2 border-white bg-primary text-[12px] font-semibold tabular-nums text-primary-foreground shadow-[var(--elevation-md)] outline-none transition-transform duration-150 hover:scale-110 focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 dark:border-zinc-900 data-[selected=true]:scale-125 data-[selected=true]:ring-4 data-[selected=true]:ring-primary/25";
  button.textContent = String(index + 1);
  button.addEventListener("click", (e) => {
    e.stopPropagation();
    onSelect(index);
  });
  // MapLibre は渡した要素の transform で位置を決めるので、拡大（scale）は内側のボタンに当てる。
  const root = document.createElement("div");
  root.appendChild(button);
  return root;
}

export type MapStatus = "idle" | "loading" | "ready" | "error";

export function useYuiMap({
  container,
  figure,
  config,
  mode,
  active,
  cooperative,
  selected,
  onSelect,
  padding,
}: {
  container: React.RefObject<HTMLDivElement | null>;
  figure: MapFigure;
  config: MapConfig;
  mode: MapMode;
  /// 画面の近くにあるか（false の間は地図を持たない）。
  active: boolean;
  /// チャットの中の地図: 1 本指のドラッグ・ホイールでページのスクロールを奪わない。
  cooperative: boolean;
  selected: number | null;
  onSelect: (index: number) => void;
  /// 地点を寄せるときに空ける余白（全画面では下に地点のカードが重なる）。
  padding?: MapPadding;
}) {
  const [status, setStatus] = React.useState<MapStatus>("idle");
  const mapRef = React.useRef<MaplibreMap | null>(null);
  const markersRef = React.useRef<Marker[]>([]);
  const libRef = React.useRef<Maplibre | null>(null);
  // 利用者が自分で動かしたら、地点が増えても視点を勝手に戻さない。
  const movedRef = React.useRef(false);
  const figureRef = React.useRef(figure);
  figureRef.current = figure;
  const modeRef = React.useRef(mode);
  modeRef.current = mode;
  const onSelectRef = React.useRef(onSelect);
  onSelectRef.current = onSelect;
  const selectedRef = React.useRef(selected);
  selectedRef.current = selected;
  const paddingRef = React.useRef(padding);
  paddingRef.current = padding;

  const styleUrl = mode === "dark" ? config.style_url_dark : config.style_url_light;
  const token = config.mapbox_token;

  // 地図の生成と破棄（画面から離れたら破棄）。
  React.useEffect(() => {
    const el = container.current;
    if (!active || !el || !config.provider || !config.style_url_light) return;
    let cancelled = false;
    setStatus("loading");
    const url = (modeRef.current === "dark" ? config.style_url_dark : config.style_url_light) ?? config.style_url_light;
    Promise.all([loadMaplibre(), loadStyle(url, token)])
      .then(([maplibre, style]) => {
        if (cancelled) return;
        libRef.current = maplibre;
        const fig = figureRef.current;
        const map = new maplibre.Map({
          container: el,
          style: withOverlay(style, modeRef.current, fig, selectedRef.current),
          transformRequest: mapboxTransform(token),
          attributionControl: { compact: true },
          cooperativeGestures: cooperative,
          dragRotate: false,
          pitchWithRotate: false,
          touchPitch: false,
          maxPitch: 0,
          fadeDuration: 150,
          locale: {
            "CooperativeGesturesHandler.WindowsHelpText": "Ctrl キーを押しながらスクロールで拡大・縮小",
            "CooperativeGesturesHandler.MacHelpText": "⌘ キーを押しながらスクロールで拡大・縮小",
            "CooperativeGesturesHandler.MobileHelpText": "2 本の指で地図を動かせます",
            "AttributionControl.ToggleAttribution": "出典を表示",
            "Map.Title": "地図",
          },
        });
        map.touchZoomRotate.disableRotation();
        mapRef.current = map;
        movedRef.current = false;
        frame(map, fig, false, paddingRef.current);
        map.on("dragstart", () => (movedRef.current = true));
        map.on("zoomstart", (e) => {
          if ((e as { originalEvent?: unknown }).originalEvent) movedRef.current = true;
        });
        map.on("click", () => onSelectRef.current(-1));
        map.once("load", () => {
          if (cancelled) return;
          // 出典は畳んだ「i」から始める（開いたままだと小さな地図の下を覆う）。
          el.querySelector(".maplibregl-ctrl-attrib")?.classList.remove("maplibregl-compact-show");
          setStatus("ready");
        });
        // スタイルが読めない（提供元の障害・トークンの失効）ときは静止画に戻す。タイル 1 枚の失敗は無視する。
        map.on("error", (e) => {
          if (!cancelled && !map.isStyleLoaded() && !map.loaded()) {
            console.warn("[map] 地図を読み込めませんでした", e.error);
            setStatus("error");
          }
        });
      })
      .catch((e: unknown) => {
        if (cancelled) return;
        console.warn("[map] 地図を読み込めませんでした", e);
        setStatus("error");
      });
    return () => {
      cancelled = true;
      for (const m of markersRef.current) m.remove();
      markersRef.current = [];
      mapRef.current?.remove();
      mapRef.current = null;
      setStatus("idle");
    };
    // mode・figure の変化は下の effect で差し替える（地図を作り直さない）。
  }, [active, container, config.provider, config.style_url_light, config.style_url_dark, token, cooperative]);

  // 明暗の切り替え: スタイルだけを差し替える（ピンは DOM なので残る）。
  const mapReady = status === "ready";
  React.useEffect(() => {
    const map = mapRef.current;
    if (!map || !mapReady || !styleUrl) return;
    let cancelled = false;
    loadStyle(styleUrl, token)
      .then((style) => {
        if (!cancelled) map.setStyle(withOverlay(style, mode, figureRef.current, selectedRef.current));
      })
      .catch((e: unknown) => console.warn("[map] スタイルを切り替えられませんでした", e));
    return () => {
      cancelled = true;
    };
  }, [mode, styleUrl, token, mapReady]);

  // ピン・道順・視点（生成中に地点が増えるたび）。
  const markerKey = React.useMemo(
    () => JSON.stringify([figure.markers.map((m) => [m.label, m.lat, m.lng]), figure.route, figure.zoom]),
    [figure.markers, figure.route, figure.zoom],
  );
  React.useEffect(() => {
    const map = mapRef.current;
    const maplibre = libRef.current;
    if (!map || !maplibre || status === "idle" || status === "error") return;
    for (const m of markersRef.current) m.remove();
    const fig = figureRef.current;
    markersRef.current = fig.markers.map((m, i) =>
      new maplibre.Marker({ element: pinElement(i, m.label, (n) => onSelectRef.current(n)), anchor: "center" })
        .setLngLat([m.lng, m.lat])
        .addTo(map),
    );
    (map.getSource(ROUTE) as GeoJSONSource | undefined)?.setData(routeData(fig));
    (map.getSource(PLACES) as GeoJSONSource | undefined)?.setData(placesData(fig));
    if (!movedRef.current) frame(map, fig, status === "ready", paddingRef.current);
  }, [markerKey, status]);

  // 選んだ地点: ピンを強調し、画面の外なら寄せる。
  React.useEffect(() => {
    const map = mapRef.current;
    markersRef.current.forEach((m, i) => {
      const button = m.getElement().querySelector("button");
      if (button) button.dataset.selected = String(i === selected);
      m.getElement().style.zIndex = i === selected ? "2" : "";
    });
    // 名前は、地点が少なければ常に、多ければ選んだものだけ出す。
    if (map?.getLayer(`${PLACES}-label`)) map.setFilter(`${PLACES}-label`, labelFilter(selected));
    if (!map || selected === null || selected < 0) return;
    const mk = figureRef.current.markers[selected];
    if (!mk) return;
    const px = map.project([mk.lng, mk.lat]);
    const { clientWidth: w, clientHeight: h } = map.getContainer();
    const bottom = Math.max(32, paddingRef.current?.bottom ?? 0);
    if (px.x < 32 || px.y < 32 || px.x > w - 32 || px.y > h - bottom) {
      map.easeTo({ center: [mk.lng, mk.lat], duration: 350, padding: { ...PADDING, ...paddingRef.current } });
    }
  }, [selected, markerKey, status]);

  const zoom = React.useCallback((delta: number) => {
    const map = mapRef.current;
    if (!map) return;
    movedRef.current = true;
    map.easeTo({ zoom: map.getZoom() + delta, duration: 200 });
  }, []);
  const recenter = React.useCallback(() => {
    const map = mapRef.current;
    if (!map) return;
    movedRef.current = false;
    frame(map, figureRef.current, true, paddingRef.current);
  }, []);

  return { status, zoom, recenter };
}

/// 要素が画面に近い（上下 1 画面ぶん以内）あいだだけ true。
export function useNearViewport(ref: React.RefObject<HTMLElement | null>): boolean {
  const [near, setNear] = React.useState(false);
  React.useEffect(() => {
    const el = ref.current;
    if (!el || typeof IntersectionObserver === "undefined") {
      setNear(true);
      return;
    }
    const io = new IntersectionObserver((entries) => setNear(entries.some((e) => e.isIntersecting)), {
      rootMargin: "100% 0px",
    });
    io.observe(el);
    return () => io.disconnect();
  }, [ref]);
  return near;
}
