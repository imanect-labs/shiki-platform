// Yui の map に背景地図を重ねて描く部品（YuiFigure の renderMap に渡す・design.md §4.7）。
//
// - 背景地図は shiki-server が配る PMTiles から読む。ブラウザは shiki 以外へ接続しない
//   （スタイルは glyphs・sprite を持たず、文字は端末のフォントで描く）。
// - 地点は点と名前の札（map-markers.ts）、道順は Yui と同じく地点を直線で結ぶ。
// - 地点の一覧は、会話・ノートの中では地図の下端に重ねたカードの列（map-list.tsx）、全画面では地図に浮かべたパネル（map-panel.tsx）。
// - 画面に近づいてから地図を作り、画面から十分離れたら破棄する（WebGL の文脈を溜めない）。
// - ページのスクロールを奪わない（Ctrl/⌘＋スクロール・2 本指で操作。全画面では通常操作）。
// - 地図データが無い・読み込めない・WebGL が使えないときは Yui の背景なしの図に戻す。
"use client";

import * as React from "react";
import { useTheme } from "next-themes";
import { Loader2, Maximize2, Minimize2, Minus, PanelLeftOpen, Plus, Scan } from "lucide-react";
import type { MapFigure } from "@imanect/yui";
import { YuiFigure } from "@imanect/yui/react";
import type { GeoJSONSource, Map as MlMap } from "maplibre-gl";
import "maplibre-gl/dist/maplibre-gl.css";

import { loadBasemapConfig, loadBasemapStyle, loadRoute, peekBasemapConfig, type BasemapConfig } from "@/lib/basemap";
import { cn } from "@/lib/utils";
import { MapCardCarousel } from "./map-list";
import { MapPanel } from "./map-panel";
import { createFigureMarkers, type FigureMarkers, type Insets } from "./map-markers";
import { initialView, routeFeature, routeQuery, routeSummary } from "./yui-map-data";

/// ピン・名前・道順の色。背景地図（青い水域・緑の森・淡い道路）のどれとも紛れない朱色にし、
/// 名前は濃い文字に太い縁取りを付けて道路の上でも読めるようにする。
const COLORS = {
  light: { pin: "#d4442e", route: "#d4442e", ink: "#1c1917", halo: "#ffffff" },
  dark: { pin: "#f06a52", route: "#f06a52", ink: "#f5f5f4", halo: "#141414" },
} as const;

/// 文字は端末のフォントで描く（Yui の図と同じ並び）。
const FONT_FAMILY =
  '"Hiragino Sans", "Hiragino Kaku Gothic ProN", "Noto Sans JP", "Noto Sans CJK JP", "Yu Gothic", Meiryo, sans-serif';

/// 全地点を収めるときの余白（地点のまわりに出す札の大きさと、左上の件数・右上の操作ボタンの分）。
const FIT_PADDING = { top: 56, bottom: 44, left: 96, right: 104 };
/// 全画面で下からパネルを出すとき、その上に置く出典の行の高さ。
const ATTRIBUTION_H = 20;
/// 地図の下端に重ねたカードの列の高さ（測る前の見込み）。
const OVERLAY_GUESS = 128;

/// これより地点の多い図は、ピンを小さくし、地図を縦に広げる。
const DENSE_MARKERS = 20;

/// 地図を作り始める距離（画面の外側）と、破棄するまでの猶予。
const LOAD_MARGIN = "300px 0px";
const DISPOSE_DELAY_MS = 2000;
/// これだけ待っても地図が出なければ背景なしの図に戻す。
const LOAD_TIMEOUT_MS = 20_000;

type Phase = "checking" | "fallback" | "idle" | "loading" | "ready";

/// 道路沿いの道順の状態（none: 直線のまま描く道順・道順なし）。
type RouteState = { status: "none" } | { status: "loading" } | { status: "ok"; summary: string } | { status: "failed" };

let protocolReady: Promise<{ add: (url: string) => void; drop: (url: string) => void }> | null = null;

/// pmtiles:// を MapLibre に登録する（ページ内で 1 回）。
function pmtilesProtocol(maplibre: typeof import("maplibre-gl")) {
  if (!protocolReady) {
    protocolReady = import("pmtiles").then(({ PMTiles, Protocol }) => {
      const protocol = new Protocol();
      maplibre.addProtocol("pmtiles", protocol.tile);
      return {
        add: (url: string) => {
          if (!protocol.get(url)) protocol.add(new PMTiles(url));
        },
        // 読み込みに失敗した地図データは捨てる（pmtiles は失敗したヘッダの読み込みを覚えたままにするので、
        // 使い回すと一時的な失敗のあと再読み込みするまで背景地図が出なくなる）。
        drop: (url: string) => {
          protocol.tiles.delete(url);
        },
      };
    });
  }
  return protocolReady;
}

function prefersReducedMotion(): boolean {
  return typeof window !== "undefined" && window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
}

export function BasemapMap({ figure, fallbackClassName }: { figure: MapFigure; fallbackClassName?: string }) {
  const { resolvedTheme } = useTheme();
  const theme = resolvedTheme === "dark" ? "dark" : "light";
  // 読み込み済みなら最初から使う（2 つめ以降の地図で、背景なしの図を一瞬挟まない）。
  const known = peekBasemapConfig();
  const [config, setConfig] = React.useState<BasemapConfig | null>(known ?? null);
  const [phase, setPhase] = React.useState<Phase>(known === undefined ? "checking" : known ? "idle" : "fallback");
  const [near, setNear] = React.useState(false);
  const [fullscreen, setFullscreen] = React.useState(false);
  const [route, setRoute] = React.useState<RouteState>({ status: "none" });
  // 要素の全画面に対応しない環境（iPhone の Safari など）ではボタンを出さない。
  const [canFullscreen, setCanFullscreen] = React.useState(false);
  const frameRef = React.useRef<HTMLDivElement>(null);
  const containerRef = React.useRef<HTMLDivElement>(null);
  const mapRef = React.useRef<MlMap | null>(null);
  const markersRef = React.useRef<FigureMarkers | null>(null);
  // 指している地点（札・一覧に乗せている）と、押して選んだ地点。カードは指している方を優先する。
  const [hover, setHoverState] = React.useState<number | null>(null);
  // 全画面の出入りの直後は、止まったままのポインタの下を札・行が通り過ぎて「指した」ことになるので、
  // しばらく指す操作を受け付けない（離れたことは届かないことがあり、指したまま残る）。
  const hoverLockUntilRef = React.useRef(0);
  const setHover = React.useCallback((i: number | null) => {
    if (i !== null && performance.now() < hoverLockUntilRef.current) return;
    setHoverState(i);
  }, []);
  const [selected, setSelected] = React.useState<number | null>(null);
  const active = hover ?? selected;
  // 離れた地点がまだ指している地点のときだけ外す（地図の札から一覧の行へ一気に移ると、
  // 行に入った後で札から離れたことが届くため）。
  const leave = React.useCallback((i: number) => setHoverState((cur) => (cur === i ? null : cur)), []);
  // 選ぶ地点が変わったら指している地点を外す（押した行・カードが表示の切り替えで消えると、
  // 離れたことが届かず前の地点を指したままになる）。
  React.useEffect(() => setHoverState(null), [selected]);
  // 会話・ノートの中では地点のカードの列を地図の下端に重ね、全画面では地点の一覧のパネルを浮かべる。
  // 地点はそれらを除いた範囲に収め、札・カードも隠れない所に置く。
  const overlayRef = React.useRef<HTMLDivElement>(null);
  const [overlayHeight, setOverlayHeight] = React.useState(OVERLAY_GUESS);
  const panelRef = React.useRef<HTMLElement>(null);
  const [panelOpen, setPanelOpen] = React.useState(true);
  const [panelInsets, setPanelInsets] = React.useState<Insets>({ left: 0, bottom: 0 });
  const insets: Insets = fullscreen
    ? panelOpen
      ? panelInsets
      : { left: 0, bottom: 0 }
    : { left: 0, bottom: overlayHeight };
  const insetsRef = React.useRef(insets);
  insetsRef.current = insets;
  /// 地図の作成・全地点への合わせ直しで使う余白（その時点の重なりの大きさで決める）。
  const fitPaddingRef = React.useRef(() => ({
    ...FIT_PADDING,
    left: Math.max(FIT_PADDING.left, insetsRef.current.left + 64),
    bottom: Math.max(FIT_PADDING.bottom, insetsRef.current.bottom + 24),
  }));
  const fitPadding = () => fitPaddingRef.current();
  /// 全画面の出入りの後、利用者が自分で地図を動かすまでは、パネル・カードの列の大きさが変わるたびに
  /// 全地点に合わせ直す（大きさが決まる前に合わせると、地点がパネルの下に入る）。
  const refitPendingRef = React.useRef(false);
  const titleId = React.useId();

  // 地図データがあるか（無ければ背景なしの図）。
  React.useEffect(() => {
    let alive = true;
    void loadBasemapConfig().then((c) => {
      if (!alive) return;
      setConfig(c);
      setPhase(c ? "idle" : "fallback");
    });
    return () => {
      alive = false;
    };
  }, []);

  // 画面に近づいたら作り、離れて少し経ったら破棄する。全画面の間は破棄しない。
  React.useEffect(() => {
    const el = frameRef.current;
    if (!el || !config) return;
    let timer: number | undefined;
    const io = new IntersectionObserver(
      ([entry]) => {
        window.clearTimeout(timer);
        if (entry?.isIntersecting) setNear(true);
        else timer = window.setTimeout(() => setNear(false), DISPOSE_DELAY_MS);
      },
      { rootMargin: LOAD_MARGIN },
    );
    io.observe(el);
    return () => {
      window.clearTimeout(timer);
      io.disconnect();
    };
  }, [config]);

  const keepAlive = near || fullscreen;

  // 地図の生成と破棄（テーマが変わったら作り直す）。
  React.useEffect(() => {
    const container = containerRef.current;
    if (!config || !keepAlive || !container) return;
    let disposed = false;
    let map: MlMap | null = null;
    let failed = false;
    let archives: string[] = [];
    setPhase("loading");
    const fail = (why: unknown) => {
      if (disposed || failed) return;
      failed = true;
      window.clearTimeout(timeout);
      // 背景なしの図に戻したら地図は捨てる（後から load・idle が来ても地図へ戻らない）。
      map?.remove();
      map = null;
      mapRef.current = null;
      void protocolReady?.then((p) => archives.forEach((u) => p.drop(u)));
      console.warn("背景地図を表示できないため、背景なしの図を表示します", why);
      setPhase("fallback");
    };
    // スタイルや部品の読み込みも含めて待つ上限（応答しない接続で「読み込み中」のまま残さない）。
    const timeout = window.setTimeout(() => fail(new Error("timeout")), LOAD_TIMEOUT_MS);
    (async () => {
      const [maplibre, loaded] = await Promise.all([
        import("maplibre-gl"),
        loadBasemapStyle(config.styleUrl, theme, window.location.origin),
      ]);
      if (disposed || failed) return;
      if (loaded.archives.length === 0) throw new Error("スタイルに地図データがありません");
      archives = loaded.archives;
      const protocol = await pmtilesProtocol(maplibre);
      loaded.archives.forEach((u) => protocol.add(u));
      if (disposed || failed) return;
      const view = initialView(figure);
      const colors = COLORS[theme];
      map = new maplibre.Map({
        container,
        style: loaded.style as unknown as import("maplibre-gl").StyleSpecification,
        ...(view.kind === "center"
          ? { center: view.center, zoom: view.zoom }
          : { bounds: view.bounds, fitBoundsOptions: { padding: fitPadding(), maxZoom: view.maxZoom } }),
        maxZoom: 18,
        attributionControl: false,
        cooperativeGestures: true,
        dragRotate: false,
        pitchWithRotate: false,
        touchPitch: false,
        localIdeographFontFamily: FONT_FAMILY,
        locale: {
          "Map.Title": figure.title ? `地図: ${figure.title}` : "地図",
          "CooperativeGesturesHandler.WindowsHelpText": "Ctrl キーを押しながらスクロールで拡大・縮小",
          "CooperativeGesturesHandler.MacHelpText": "⌘ キーを押しながらスクロールで拡大・縮小",
          "CooperativeGesturesHandler.MobileHelpText": "2 本の指で地図を動かします",
        },
      });
      mapRef.current = map;
      const m = map;
      // 枠の大きさが変わったら（一覧の表示・幅の変化）地図を合わせる（MapLibre が見るのは窓の大きさだけ）。
      const ro = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(() => m.resize());
      ro?.observe(container);
      m.once("remove", () => ro?.disconnect());
      m.touchZoomRotate.disableRotation();
      m.keyboard.disableRotation();
      // 全画面のまま作り直したとき（テーマの切り替えなど）も、全画面ではスクロールで拡大縮小できるようにする。
      if (document.fullscreenElement === frameRef.current) m.cooperativeGestures.disable();
      let loadedOnce = false;
      m.on("error", (e) => {
        // 読み込み前の失敗（スタイル・地図データ）は背景なしの図に戻す。読み込み後のタイル単位の
        // 失敗は、その範囲が空くだけなので図はそのまま見せる。
        if (!loadedOnce) fail(e.error);
      });
      m.on("load", () => {
        loadedOnce = true;
        // 枠の大きさが決まってから全地点に合わせ直す（作った時点では一覧の幅が反映されていないことがある）。
        m.resize();
        if (view.kind === "bounds") m.fitBounds(view.bounds, { padding: fitPadding(), maxZoom: view.maxZoom, animate: false });
        addRouteLayer(m, figure, colors);
        markersRef.current = createFigureMarkers(maplibre, m, figure, {
          onHover: setHover,
          onLeave: leave,
          onSelect: (i) => setSelected((cur) => (cur === i ? null : i)),
        });
        // 地図の何も無い所を押したら選んだ地点を外す（札の上の押下は札が受ける）。
        m.on("click", (e) => {
          if (e.originalEvent.target === m.getCanvas()) setSelected(null);
        });
        // 利用者が自分で動かしたら（ドラッグ・ホイール・ボタン・地点へ寄る）、以後は勝手に合わせ直さない。
        m.on("movestart", (e) => {
          if ((e as { originalEvent?: Event }).originalEvent) refitPendingRef.current = false;
        });
        m.once("idle", () => {
          window.clearTimeout(timeout);
          if (!disposed && !failed) setPhase("ready");
        });
        // 道順は、まず Yui と同じ直線で描き、道路沿いの線が取れたら差し替える（取れなければ直線のまま）。
        const query = config.routing && m.getSource("yui-route") ? routeQuery(figure) : null;
        if (!query) {
          setRoute({ status: "none" });
          return;
        }
        setRoute({ status: "loading" });
        loadRoute(query)
          .then((r) => {
            if (disposed || failed || mapRef.current !== m) return;
            if (!r) {
              setRoute({ status: "failed" });
              return;
            }
            (m.getSource("yui-route") as GeoJSONSource | undefined)?.setData({
              type: "Feature",
              geometry: { type: "LineString", coordinates: r.coordinates },
              properties: {},
            });
            // 道路沿いの線は実線で描く（直線の破線と見分ける）。
            m.setPaintProperty("yui-route", "line-dasharray", undefined);
            m.setPaintProperty("yui-route", "line-width", 4);
            setRoute({ status: "ok", summary: routeSummary(query.mode, r.distance_m, r.duration_s) });
          })
          .catch(() => {
            if (!disposed) setRoute({ status: "failed" });
          });
      });
    })().catch(fail);
    return () => {
      disposed = true;
      window.clearTimeout(timeout);
      markersRef.current?.remove();
      markersRef.current = null;
      setHoverState(null);
      setSelected(null);
      map?.remove();
      mapRef.current = null;
      // 作り直す地図は直線から描き直すので、前の地図の道順の表示を残さない。
      setRoute({ status: "none" });
      setPhase((p) => (p === "fallback" ? p : "idle"));
    };
  }, [config, keepAlive, theme, figure, leave, setHover]);

  // 指している・選んでいる地点を札とカードに映す。
  React.useEffect(() => {
    markersRef.current?.setActive(active);
  }, [active, phase]);

  // カードの列の高さを測る（文字の大きさ・説明の行数で変わる）。
  React.useEffect(() => {
    const el = overlayRef.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(() => setOverlayHeight(Math.round(el.offsetHeight)));
    ro.observe(el);
    return () => ro.disconnect();
  }, [fullscreen, phase]);
  // 全画面のパネルの大きさを測る（広い画面は左に浮かべ、狭い画面は下から出す）。
  React.useEffect(() => {
    const el = panelRef.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    const measure = () => {
      const box = el.parentElement;
      if (!box) return;
      const side = el.offsetWidth < box.clientWidth * 0.7;
      const next = side
        ? { left: el.offsetLeft + el.offsetWidth + 8, bottom: 0 }
        : { left: 0, bottom: box.clientHeight - el.offsetTop + 8 + ATTRIBUTION_H };
      setPanelInsets((cur) => (cur.left === next.left && cur.bottom === next.bottom ? cur : next));
    };
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    if (el.parentElement) ro.observe(el.parentElement);
    return () => ro.disconnect();
  }, [fullscreen, panelOpen, phase]);
  React.useEffect(() => {
    markersRef.current?.setInsets(insets);
    // 全画面の出入りの後で、まだ利用者が地図を動かしていなければ、全地点に合わせ直す。
    const m = mapRef.current;
    if (!refitPendingRef.current || !m) return;
    m.resize();
    const v = initialView(figure);
    if (v.kind === "bounds") m.fitBounds(v.bounds, { padding: fitPaddingRef.current(), maxZoom: v.maxZoom, animate: false });
  }, [insets.left, insets.bottom, phase, figure]); // eslint-disable-line react-hooks/exhaustive-deps
  // 左上の件数・右上の操作ボタンの大きさ（札とカードがその下に潜らないように）。
  const countRef = React.useRef<HTMLDivElement>(null);
  const controlsRef = React.useRef<HTMLDivElement>(null);
  React.useEffect(() => {
    const size = (el: HTMLElement | null): [number, number] =>
      el ? [el.offsetLeft + el.offsetWidth + 4, el.offsetTop + el.offsetHeight + 4] : [0, 0];
    const box = (el: HTMLElement | null): [number, number] => {
      if (!el?.parentElement) return [0, 0];
      // 右上の部品は右端からの幅にする。
      return [el.parentElement.clientWidth - el.offsetLeft + 4, el.offsetTop + el.offsetHeight + 4];
    };
    markersRef.current?.setCorners({ topLeft: size(countRef.current), topRight: box(controlsRef.current) });
  }, [phase, fullscreen, canFullscreen, panelOpen]);

  // 全画面の出入り。全画面の間はスクロールで拡大縮小できるようにする。
  React.useEffect(() => {
    setCanFullscreen(document.fullscreenEnabled === true);
    const onChange = () => {
      const on = document.fullscreenElement === frameRef.current;
      setFullscreen(on);
      hoverLockUntilRef.current = performance.now() + 800;
      setHoverState(null);
      const m = mapRef.current;
      if (!m) return;
      if (on) m.cooperativeGestures.disable();
      else m.cooperativeGestures.enable();
      // 大きさが変わったら全地点に合わせ直す（全画面で余白だらけ・小さく固まって見えないように）。
      refitPendingRef.current = true;
      requestAnimationFrame(() => {
        m.resize();
        const v = initialView(figure);
        if (v.kind === "bounds") m.fitBounds(v.bounds, { padding: fitPaddingRef.current(), maxZoom: v.maxZoom, animate: false });
      });
    };
    document.addEventListener("fullscreenchange", onChange);
    return () => document.removeEventListener("fullscreenchange", onChange);
  }, [figure]);

  if (phase === "fallback" || phase === "checking") {
    // 設定を読む間も背景なしの図を出しておく（地図データが無い環境ではそのまま）。
    return <YuiFigure figure={figure} {...(fallbackClassName ? { className: fallbackClassName } : {})} />;
  }

  const ready = phase === "ready";
  const animate = !prefersReducedMotion();
  const zoomBy = (d: number) => {
    refitPendingRef.current = false;
    mapRef.current?.zoomTo(mapRef.current.getZoom() + d, { animate });
  };
  const fitAll = () => {
    const m = mapRef.current;
    if (!m) return;
    const v = initialView(figure);
    if (v.kind === "center") m.easeTo({ center: v.center, zoom: v.zoom, animate });
    else m.fitBounds(v.bounds, { padding: fitPadding(), maxZoom: v.maxZoom, animate });
  };
  const toggleFullscreen = () => {
    const done = document.fullscreenElement ? document.exitFullscreen() : frameRef.current?.requestFullscreen();
    void done?.catch(() => {
      /* 権限などで全画面にできないときは何もしない */
    });
  };
  const focusMarker = (i: number) => {
    const m = mapRef.current;
    const mk = figure.markers[i];
    if (!m || !mk) return;
    refitPendingRef.current = false;
    setSelected(i);
    // 見えていないとき（画面の外・カードの列やパネルの下）だけ寄せる（見えているなら、ほかの地点との位置関係を崩さない）。
    const pt = m.project([mk.lng, mk.lat]);
    const canvas = m.getCanvas();
    const hidden =
      pt.x < insets.left + 8 || pt.x > canvas.clientWidth - 8 || pt.y < 8 || pt.y > canvas.clientHeight - insets.bottom - 8;
    // 寄せる先は、カードの列・パネルを除いた範囲の真ん中。
    if (hidden) m.easeTo({ center: [mk.lng, mk.lat], offset: [insets.left / 2, -insets.bottom / 2], animate });
  };
  /// その地点へ寄る（拡大して、パネルを除いた範囲の真ん中に）。
  const zoomTo = (i: number) => {
    const m = mapRef.current;
    const mk = figure.markers[i];
    if (!m || !mk) return;
    refitPendingRef.current = false;
    setSelected(i);
    m.easeTo({
      center: [mk.lng, mk.lat],
      zoom: Math.max(m.getZoom(), 16),
      offset: [insets.left / 2, -insets.bottom / 2],
      animate,
      duration: 900,
    });
  };

  const listProps = {
    markers: figure.markers,
    active,
    selected,
    ready,
    onHover: setHover,
    onLeave: leave,
    onPick: focusMarker,
  };

  const attribution = (
    <div
      className={cn(
        "max-w-full truncate text-[10px] leading-4 text-muted-foreground",
        // 全画面では地図の右下に重ねる。会話の中ではカードの列の下に置く（列に隠れない）。
        fullscreen ? "absolute right-0 z-20 rounded-tl-lg bg-background/75 px-2 py-0.5 backdrop-blur" : "px-3 text-right",
      )}
      // 全画面で下からパネルを出しているときは、その上に出す（隠さない）。
      style={fullscreen ? { bottom: Math.max(0, insets.bottom - ATTRIBUTION_H) } : undefined}
      data-testid="basemap-attribution"
    >
      {config?.attribution.map((a, i) => (
        <React.Fragment key={a.url}>
          {i > 0 ? " " : null}
          <a href={a.url} target="_blank" rel="noopener noreferrer" className="text-inherit no-underline hover:underline">
            {a.text}
          </a>
        </React.Fragment>
      ))}
    </div>
  );

  return (
    <div
      className="min-w-0"
      data-testid="basemap-figure"
      data-state={phase}
      data-list={fullscreen ? "panel" : "overlay"}
      // 道順の線が道路沿いか直線か（見た目と同じ区別を E2E でも確かめる）。
      data-route={route.status === "ok" ? "road" : figure.route ? "straight" : "none"}
    >
      {figure.title ? (
        <div id={titleId} className="mb-2 text-[14px] font-semibold leading-snug text-foreground">
          {figure.title}
        </div>
      ) : null}
      {route.status !== "none" ? (
        <p className="-mt-1 mb-2 text-[12px] leading-5 text-muted-foreground" data-testid="basemap-route" aria-live="polite">
          {route.status === "ok"
            ? `道順: ${route.summary}（道路沿い）`
            : route.status === "loading"
              ? "道順を計算しています…"
              : "道路沿いの道順を出せないため、地点を直線で結んでいます"}
        </p>
      ) : null}
      {/* 地図と地点の一覧を 1 枚にまとめる。会話やノートの中では地図の下端にカードの列を重ね（本文の幅の中で
          地図を横に狭めず、縦にも伸ばしすぎない）、全画面では一覧のパネルを地図の上に浮かべる。 */}
      <div
        ref={frameRef}
        role="region"
        aria-labelledby={figure.title ? titleId : undefined}
        aria-label={figure.title ? undefined : "地図"}
        className={cn(
          "flex overflow-hidden bg-background",
          fullscreen
            ? "h-screen w-screen"
            : "flex-col rounded-2xl border border-border/50 shadow-[0_1px_2px_rgba(0,0,0,0.04),0_4px_16px_rgba(0,0,0,0.04)]",
        )}
      >
        <div
          className={cn(
            "relative min-w-0 bg-secondary/40",
            // 会話・ノートの中は高さを決め打ち（カードの列の分も含む）、全画面は画面いっぱい（一覧は地図の上に浮かべる）。
            fullscreen
              ? "h-full w-full"
              : figure.markers.length > DENSE_MARKERS
                ? "h-[460px] sm:h-[540px]"
                : "h-[400px] sm:h-[460px]",
          )}
        >
          {/* MapLibre は自分の要素に position: relative を付けるので、絶対配置ではなく大きさで埋める。 */}
          <div ref={containerRef} className="h-full w-full" data-testid="basemap-canvas" />
          {!ready ? (
            <div
              className="pointer-events-none absolute inset-0 z-30 flex items-center justify-center gap-2 bg-secondary/60 text-xs text-muted-foreground"
              aria-live="polite"
            >
              <Loader2 className="size-3.5 animate-spin" />
              地図を読み込んでいます…
            </div>
          ) : null}
          {!fullscreen ? (
            <div
              ref={countRef}
              className="pointer-events-none absolute left-3 top-3 z-20 rounded-full bg-background/90 px-3 py-1 text-[12px] font-semibold tabular-nums text-foreground shadow-[0_2px_10px_rgba(0,0,0,0.12)] ring-1 ring-black/5 backdrop-blur-md dark:ring-white/10"
              aria-hidden="true"
            >
              {figure.markers.length} か所
            </div>
          ) : panelOpen ? (
            <MapPanel
              ref={panelRef}
              {...listProps}
              title={figure.title}
              note={
                route.status === "ok"
                  ? `道順: ${route.summary}（道路沿い）`
                  : route.status === "failed"
                    ? "道路沿いの道順を出せないため、地点を直線で結んでいます"
                    : undefined
              }
              onClear={() => setSelected(null)}
              onZoomTo={zoomTo}
              onCollapse={() => setPanelOpen(false)}
            />
          ) : (
            // 畳んだパネルを開くボタン（地図の左上）。
            <div ref={countRef} className="absolute left-4 top-4 z-30">
              <button
                type="button"
                onClick={() => setPanelOpen(true)}
                className="flex h-10 items-center gap-2 rounded-full bg-background/95 pl-3 pr-4 text-[13px] font-semibold text-foreground shadow-[0_6px_24px_rgba(0,0,0,0.16)] ring-1 ring-black/5 backdrop-blur-xl transition-colors hover:bg-background focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring dark:ring-white/10"
              >
                <PanelLeftOpen className="size-4" />
                一覧を表示
                <span className="tabular-nums text-muted-foreground">{figure.markers.length} か所</span>
              </button>
            </div>
          )}
          <div
            ref={controlsRef}
            className={cn(
              "absolute z-20 flex flex-col overflow-hidden bg-background/90 shadow-[0_2px_10px_rgba(0,0,0,0.12)] ring-1 ring-black/5 backdrop-blur-md dark:ring-white/10",
              fullscreen ? "right-4 top-4 rounded-2xl shadow-[0_6px_24px_rgba(0,0,0,0.16)]" : "right-3 top-3 rounded-xl",
            )}
          >
            <MapButton label="拡大" onClick={() => zoomBy(1)} disabled={!ready}>
              <Plus className="size-4" />
            </MapButton>
            <MapButton label="縮小" onClick={() => zoomBy(-1)} disabled={!ready} className="border-t border-border/50">
              <Minus className="size-4" />
            </MapButton>
            <MapButton label="全地点を表示" onClick={fitAll} disabled={!ready} className="border-t border-border/50">
              <Scan className="size-4" />
            </MapButton>
            {canFullscreen ? (
              <MapButton
                label={fullscreen ? "全画面を終了" : "全画面で表示"}
                onClick={toggleFullscreen}
                className="border-t border-border/50"
                data-testid="basemap-fullscreen"
              >
                {fullscreen ? <Minimize2 className="size-4" /> : <Maximize2 className="size-4" />}
              </MapButton>
            ) : null}
          </div>
          {/* 出典（地図データの利用条件）は閉じたり隠したりできない形で常に出す。 */}
          {fullscreen ? (
            attribution
          ) : (
            // 地図の下端に重ねるカードの列。下を背景色で薄くぼかして地図と分け、列の下に出典を置く。
            <div
              ref={overlayRef}
              className="absolute inset-x-0 bottom-0 z-20 flex flex-col gap-1.5 bg-gradient-to-t from-background from-[22px] via-background/60 via-55% to-transparent pb-1.5 pt-8"
            >
              <MapCardCarousel {...listProps} />
              {attribution}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

function MapButton({
  label,
  className,
  children,
  ...props
}: React.ComponentProps<"button"> & { label: string }) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      className={cn(
        "flex size-9 items-center justify-center text-foreground/75 transition-colors hover:bg-foreground/5 hover:text-foreground",
        "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring disabled:opacity-40 disabled:hover:bg-transparent",
        className,
      )}
      {...props}
    >
      {children}
    </button>
  );
}

/// 道順の線のレイヤを足す（地点の札は DOM で描く・map-markers.ts）。
function addRouteLayer(m: MlMap, figure: MapFigure, colors: (typeof COLORS)["light"] | (typeof COLORS)["dark"]) {
  const route = routeFeature(figure);
  if (!route) return;
  m.addSource("yui-route", { type: "geojson", data: route });
  m.addLayer({
    id: "yui-route",
    type: "line",
    source: "yui-route",
    layout: { "line-cap": "round", "line-join": "round" },
    paint: { "line-color": colors.route, "line-width": 3.5, "line-opacity": 0.85, "line-dasharray": [2, 1.6] },
  });
}
