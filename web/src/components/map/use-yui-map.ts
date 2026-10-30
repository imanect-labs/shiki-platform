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
import { MAP_PALETTE, ROUTE_COLOR, tintStyle, type MapMode } from "./map-style";

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
function loadStyle(url: string): Promise<StyleSpecification> {
  let p = styles.get(url);
  if (!p) {
    p = fetch(url)
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
/// ピンの下に敷く透明な印の名前と大きさ（ピンは 24px）。
const PIN_HOLE = "yui-pin-hole";
const PIN_HOLE_PX = 24;
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
        // 名前はピンの下を基本に、ほかのピン・名前とぶつかれば上・左・右へ逃がす。
        "text-variable-anchor": ["top", "bottom", "left", "right"],
        "text-radial-offset": 1.45,
        "text-justify": "auto",
        "text-max-width": 9,
        "symbol-sort-key": ["get", "i"],
      },
      paint: { "text-color": p.labelStrong, "text-halo-color": p.halo, "text-halo-width": 1.8 },
    },
    {
      // ピンの大きさの透明な印。名前より上に置いて先に場所を取り、地図側の文字と地点の名前を
      // ピンに重ねない（ピン自体は押せる部品なので DOM で描く）。
      id: `${PLACES}-hole`,
      type: "symbol",
      source: PLACES,
      layout: { "icon-image": PIN_HOLE, "icon-allow-overlap": true },
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
    "flex size-6 items-center justify-center rounded-full border-2 border-white bg-primary text-[11px] font-semibold tabular-nums text-primary-foreground shadow-[var(--elevation-md)] outline-none transition-transform duration-[var(--duration-fast)] ease-[var(--ease-standard)] hover:scale-110 focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 dark:border-zinc-900 data-[selected=true]:scale-[1.3] data-[selected=true]:shadow-[var(--elevation-lg)]";
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

/// ピンどうしがこれより近いと重なって番号が読めない（ピンは 24px）。
const PIN_GAP = 22;
/// 離すときの上限（ずらしすぎると本当の位置から離れて見える）。
const PIN_NUDGE_MAX = 10;

/// 画面上で近すぎるピンだけを、互いに反対向きへ少しずらす（番号を読めるようにする）。
/// 名前と透明な印は本当の位置に描くので、ずらす量は名前との間隔（text-radial-offset）より小さく抑える。
function declutter(map: MaplibreMap, markers: Marker[]) {
  const pts = markers.map((m) => map.project(m.getLngLat()));
  const off = pts.map(() => ({ x: 0, y: 0 }));
  for (let i = 0; i < pts.length; i++) {
    for (let j = i + 1; j < pts.length; j++) {
      const a = pts[i]!;
      const b = pts[j]!;
      let dx = b.x - a.x;
      let dy = b.y - a.y;
      const d = Math.hypot(dx, dy);
      if (d >= PIN_GAP) continue;
      // 同じ座標なら左右に分ける。
      if (d < 0.5) [dx, dy] = [1, 0];
      const len = Math.hypot(dx, dy);
      const push = Math.min(PIN_NUDGE_MAX, (PIN_GAP - d) / 2);
      off[i]!.x -= (dx / len) * push;
      off[i]!.y -= (dy / len) * push;
      off[j]!.x += (dx / len) * push;
      off[j]!.y += (dy / len) * push;
    }
  }
  markers.forEach((m, i) => {
    const o = off[i]!;
    const x = Math.max(-PIN_NUDGE_MAX, Math.min(PIN_NUDGE_MAX, o.x));
    const y = Math.max(-PIN_NUDGE_MAX, Math.min(PIN_NUDGE_MAX, o.y));
    m.setOffset([Math.round(x), Math.round(y)]);
  });
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
  attribution = "bottom-left",
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
  /// 出典を置く角。
  attribution?: "bottom-left" | "top-left";
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
  /// 地図に当てている明暗（作ったとき・切り替えたとき）。
  const appliedModeRef = React.useRef<MapMode | null>(null);
  const selectedRef = React.useRef(selected);
  selectedRef.current = selected;
  const paddingRef = React.useRef(padding);
  paddingRef.current = padding;

  const styleUrl = mode === "dark" ? config.style_url_dark : config.style_url_light;

  // 地図の生成と破棄（画面から離れたら破棄）。
  React.useEffect(() => {
    const el = container.current;
    if (!active || !el || !config.style_url_light) return;
    let cancelled = false;
    setStatus("loading");
    const url = (modeRef.current === "dark" ? config.style_url_dark : config.style_url_light) ?? config.style_url_light;
    Promise.all([loadMaplibre(), loadStyle(url)])
      .then(([maplibre, style]) => {
        if (cancelled) return;
        libRef.current = maplibre;
        const fig = figureRef.current;
        appliedModeRef.current = modeRef.current;
        const map = new maplibre.Map({
          container: el,
          style: withOverlay(style, modeRef.current, fig, selectedRef.current),
          attributionControl: false,
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
        // 出典は左下に畳んで置く（右下は操作ボタン）。
        map.addControl(new maplibre.AttributionControl({ compact: true }), attribution);
        // ピンの下の透明な印は、スタイルを差し替えるたびに求められる。
        map.on("styleimagemissing", (e) => {
          if (e.id !== PIN_HOLE || map.hasImage(PIN_HOLE)) return;
          map.addImage(PIN_HOLE, { width: PIN_HOLE_PX, height: PIN_HOLE_PX, data: new Uint8Array(PIN_HOLE_PX * PIN_HOLE_PX * 4) });
        });
        // 一度描けたら、以後の失敗（明暗の切り替え中のタイルなど）で静止画に戻さない。
        let loaded = false;
        mapRef.current = map;
        movedRef.current = false;
        frame(map, fig, false, paddingRef.current);
        map.on("dragstart", () => (movedRef.current = true));
        // 縮尺が変わるとピンどうしの近さも変わる。
        map.on("zoomend", () => declutter(map, markersRef.current));
        map.on("zoomstart", (e) => {
          if ((e as { originalEvent?: unknown }).originalEvent) movedRef.current = true;
        });
        map.on("click", () => onSelectRef.current(-1));
        map.once("load", () => {
          loaded = true;
          if (cancelled) return;
          // 出典は畳んだ「i」から始める（開いたままだと小さな地図の下を覆う）。
          el.querySelector(".maplibregl-ctrl-attrib")?.classList.remove("maplibregl-compact-show");
          setStatus("ready");
        });
        // スタイルが読めない（提供元の障害・トークンの失効）ときは静止画に戻す。タイル 1 枚の失敗は無視する。
        map.on("error", (e) => {
          // タイル・ソース単位の失敗（500・429・通信断）は、その部分が欠けるだけなので地図は残す。
          const scoped = e as { tile?: unknown; sourceId?: unknown };
          if (cancelled || loaded || scoped.tile || scoped.sourceId) return;
          console.warn("[map] 地図を読み込めませんでした", e.error);
          setStatus("error");
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
  }, [active, container, config.style_url_light, config.style_url_dark, cooperative, attribution]);

  // 明暗の切り替え: スタイルだけを差し替える（ピンは DOM なので残る）。作ったときの明暗のままなら何もしない。
  const mapReady = status === "ready";
  React.useEffect(() => {
    const map = mapRef.current;
    if (!map || !mapReady || !styleUrl || appliedModeRef.current === mode) return;
    appliedModeRef.current = mode;
    let cancelled = false;
    loadStyle(styleUrl)
      .then((style) => {
        if (!cancelled) map.setStyle(withOverlay(style, mode, figureRef.current, selectedRef.current));
      })
      .catch((e: unknown) => console.warn("[map] スタイルを切り替えられませんでした", e));
    return () => {
      cancelled = true;
    };
  }, [mode, styleUrl, mapReady]);

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
    declutter(map, markersRef.current);
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
/// いちばん近い縦にスクロールする祖先（無ければ null＝画面）。
function scrollParent(el: HTMLElement): HTMLElement | null {
  for (let p = el.parentElement; p; p = p.parentElement) {
    const y = getComputedStyle(p).overflowY;
    if (y === "auto" || y === "scroll" || y === "overlay") return p;
  }
  return null;
}

export function useNearViewport(ref: React.RefObject<HTMLElement | null>): boolean {
  const [near, setNear] = React.useState(false);
  React.useEffect(() => {
    const el = ref.current;
    if (!el || typeof IntersectionObserver === "undefined") {
      setNear(true);
      return;
    }
    // 余白（rootMargin）は root にしか効かない。会話は自前のスクロール領域の中で動くので、
    // 画面（viewport）を root にすると領域の外の地図は余白に関係なく見えない扱いになる。
    const io = new IntersectionObserver((entries) => setNear(entries.some((e) => e.isIntersecting)), {
      root: scrollParent(el),
      rootMargin: "100% 0px",
    });
    io.observe(el);
    return () => io.disconnect();
  }, [ref]);
  return near;
}
