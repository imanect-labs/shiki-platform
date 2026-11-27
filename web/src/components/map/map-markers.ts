// 地図の上の地点（位置の点と名前の札）と、地点の詳しい内容のカード（MapLibre の Marker・DOM で描く）。
//
// - 地点には小さな点を置き、名前は白い角丸の札で点の上・右・左・下のうち空いている所に出す。
//   どこに置いてもほかの札・点と重なるときは札を出さず点だけにする（一覧の順で先の地点を優先）。
// - 指した（マウスを乗せた・キーボードで移った）地点・選んだ地点は札を黒くし、カードを出す。
//
// 名前・説明は回答やノートの文字なので、HTML として解釈させない（textContent で入れる）。
import type { MapFigure } from "@imanect/yui";
import type { Map as MlMap, Marker } from "maplibre-gl";

/// 札の高さと、点から札までの間隔。
const TAG_H = 28;
const GAP = 8;

type Placement = "above" | "right" | "left" | "below" | "none";

/// 札の置き場所ごとの位置（点が原点）。
const PLACE_CLASS: Record<Exclude<Placement, "none">, string> = {
  above: "bottom-[8px] left-1/2 -translate-x-1/2",
  right: "left-[8px] top-1/2 -translate-y-1/2",
  left: "right-[8px] top-1/2 -translate-y-1/2",
  below: "top-[8px] left-1/2 -translate-x-1/2",
};

const TAG_BASE =
  "absolute flex h-[28px] max-w-[180px] items-center whitespace-nowrap rounded-full px-3 text-[12.5px] font-semibold tracking-tight " +
  "leading-none shadow-[0_2px_8px_rgba(0,0,0,0.16)] ring-1 transition-[background-color,color,transform] duration-150 " +
  "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring";
const TAG_IDLE = "bg-background text-foreground ring-black/5 hover:scale-[1.04] dark:ring-white/10";
const TAG_ACTIVE = "scale-[1.06] bg-foreground text-background ring-foreground";
const DOT_BASE =
  "absolute left-0 top-0 -translate-x-1/2 -translate-y-1/2 cursor-pointer rounded-full border-[2.5px] border-white " +
  "shadow-[0_1px_4px_rgba(0,0,0,0.4)] transition-[width,height] duration-150";

export type MarkerHandlers = {
  /// 地点を指した。
  onHover: (i: number) => void;
  /// 地点から離れた（指す先がもうほかの地点に移っていれば何もしない）。
  onLeave: (i: number) => void;
  /// 地点を押した。
  onSelect: (i: number) => void;
};

/// 地点の札とカードをまとめて扱う。
export type FigureMarkers = {
  /// 指している・選んでいる地点を強調し、カードを出す（null で消す）。
  setActive: (i: number | null) => void;
  /// 地図の左端・下端に重なっている部品（全画面の一覧のパネル・カードの列）の幅と高さ。
  /// 札とカードはそれを除いた範囲だけに置く。
  setInsets: (insets: Insets) => void;
  /// 地図の左上・右上に重なっている部品（件数・操作ボタン）の大きさ（余白込み）。札とカードはそこを避ける。
  setCorners: (corners: Corners) => void;
  remove: () => void;
};

type Entry = {
  marker: Marker;
  root: HTMLDivElement;
  tag: HTMLButtonElement;
  dot: HTMLSpanElement;
  lng: number;
  lat: number;
  width: number;
  placement: Placement;
};

type Box = { x0: number; y0: number; x1: number; y1: number };

/// 地図の左端・下端に重なる部品の幅と高さ。
export type Insets = { left: number; bottom: number };

/// 角に重なる部品の幅と高さ（[幅, 高さ]）。
export type Corners = { topLeft: [number, number]; topRight: [number, number] };

function tagBox(placement: Exclude<Placement, "none">, x: number, y: number, w: number): Box {
  switch (placement) {
    case "above":
      return { x0: x - w / 2, x1: x + w / 2, y0: y - GAP - TAG_H, y1: y - GAP };
    case "below":
      return { x0: x - w / 2, x1: x + w / 2, y0: y + GAP, y1: y + GAP + TAG_H };
    case "right":
      return { x0: x + GAP, x1: x + GAP + w, y0: y - TAG_H / 2, y1: y + TAG_H / 2 };
    case "left":
      return { x0: x - GAP - w, x1: x - GAP, y0: y - TAG_H / 2, y1: y + TAG_H / 2 };
  }
}

const overlaps = (a: Box, b: Box, m = 3) =>
  a.x0 - m < b.x1 && b.x0 - m < a.x1 && a.y0 - m < b.y1 && b.y0 - m < a.y1;

export function createFigureMarkers(
  maplibre: typeof import("maplibre-gl"),
  map: MlMap,
  figure: MapFigure,
  handlers: MarkerHandlers,
): FigureMarkers {
  let active: number | null = null;
  const entries: Entry[] = figure.markers.map((mk, i) => {
    // MapLibre に渡す要素（大きさ 0 の点）。位置は MapLibre が class と transform で決めるので触らない。
    const root = document.createElement("div");
    root.addEventListener("mouseenter", () => handlers.onHover(i));
    root.addEventListener("mouseleave", () => handlers.onLeave(i));
    const dot = document.createElement("span");
    dot.setAttribute("aria-hidden", "true");
    const tag = document.createElement("button");
    tag.type = "button";
    tag.dataset.testid = "basemap-tag";
    tag.setAttribute("aria-label", mk.desc ? `${mk.label}（${mk.desc}）` : mk.label);
    tag.addEventListener("focus", () => handlers.onHover(i));
    tag.addEventListener("blur", () => handlers.onLeave(i));
    const select = (e: Event) => {
      e.stopPropagation();
      handlers.onSelect(i);
    };
    tag.addEventListener("click", select);
    dot.addEventListener("click", select);
    const text = document.createElement("span");
    text.className = "truncate";
    text.textContent = mk.label;
    tag.append(text);
    root.append(dot, tag);
    const marker = new maplibre.Marker({ element: root, anchor: "center" })
      .setLngLat([mk.lng, mk.lat])
      .addTo(map);
    return { marker, root, tag, dot, lng: mk.lng, lat: mk.lat, width: 0, placement: "above" };
  });

  const style = (i: number) => {
    const e = entries[i];
    if (!e) return;
    const on = active === i;
    // 札を出せない地点も、指している間は点の上に出す。
    const placement: Placement = e.placement === "none" && on ? "above" : e.placement;
    e.tag.className =
      placement === "none" ? "hidden" : `${TAG_BASE} ${PLACE_CLASS[placement]} ${on ? TAG_ACTIVE : TAG_IDLE}`;
    e.dot.className = `${DOT_BASE} ${on ? "size-4 bg-foreground" : "size-3 bg-[#e5484d]"}`;
    // 一覧の順で先の地点、指している地点を上に重ねる。
    e.root.style.zIndex = String(on ? 1000 : placement === "none" ? 1 : 500 - i);
  };

  // 札の置き場所を決める（一覧の順で先の地点から、上・右・左・下の順に空いている所へ）。
  let frame = 0;
  let insets: Insets = { left: 0, bottom: 0 };
  let corners: Corners = { topLeft: [0, 0], topRight: [0, 0] };
  /// 角の部品が占める範囲。
  const cornerBoxes = (view: Box): Box[] => [
    { x0: view.x0, y0: view.y0, x1: view.x0 + corners.topLeft[0], y1: view.y0 + corners.topLeft[1] },
    { x0: view.x1 - corners.topRight[0], y0: view.y0, x1: view.x1, y1: view.y0 + corners.topRight[1] },
  ];
  /// 札・カードを置ける範囲（地図の左端・下端に重なる部品の分を除く）。
  const visible = (): Box => {
    const canvas = map.getCanvas();
    return {
      x0: Math.min(insets.left, canvas.clientWidth),
      y0: 0,
      x1: canvas.clientWidth,
      y1: Math.max(0, canvas.clientHeight - insets.bottom),
    };
  };
  // 動かしている間もカードを枠の中に保つ（カードを作った後で入れる）。
  const follow: { card?: () => void } = {};
  const layout = () => {
    frame = 0;
    const view = visible();
    const blocked = cornerBoxes(view);
    const pts = entries.map((e) => map.project([e.lng, e.lat]));
    // 点はすべて障害物（札で点を隠さない）。
    const dots: Box[] = pts.map((p) => ({ x0: p.x - 6, x1: p.x + 6, y0: p.y - 6, y1: p.y + 6 }));
    const tags: Box[] = [];
    entries.forEach((e, i) => {
      const p = pts[i];
      if (!p) return;
      const inView = p.x >= view.x0 && p.x <= view.x1 && p.y >= view.y0 && p.y <= view.y1;
      let next: Placement = "none";
      if (inView) {
        for (const c of ["above", "right", "left", "below"] as const) {
          const box = tagBox(c, p.x, p.y, e.width);
          const inside = box.x0 >= view.x0 && box.x1 <= view.x1 && box.y0 >= view.y0 && box.y1 <= view.y1;
          const free =
            !tags.some((b) => overlaps(b, box)) &&
            !dots.some((b, j) => j !== i && overlaps(b, box, 1)) &&
            !blocked.some((b) => overlaps(b, box, 0));
          if (inside && free) {
            tags.push(box);
            next = c;
            break;
          }
        }
      }
      if (next !== e.placement) {
        e.placement = next;
        style(i);
      }
    });
    follow.card?.();
  };
  const schedule = () => {
    if (!frame) frame = requestAnimationFrame(layout);
  };
  entries.forEach((_, i) => style(i));
  // 札の幅を測ってから置き場所を決める（測れなければ文字数から見積もる）。
  entries.forEach((e, i) => {
    e.width = Math.min(180, e.tag.offsetWidth || 22 + (figure.markers[i]?.label.length ?? 0) * 12.5);
  });
  layout();
  // 描き直すたびに見直す（移動・拡大縮小・枠の大きさの変化のどれでも札の位置が変わる）。
  map.on("render", schedule);

  // カード（指している地点の名前と説明）。
  const card = document.createElement("div");
  card.dataset.testid = "basemap-card";
  card.setAttribute("role", "status");
  card.className =
    "pointer-events-none w-max max-w-[280px] rounded-2xl bg-background/95 px-4 py-3 text-left shadow-[0_8px_30px_rgba(0,0,0,0.18)] ring-1 ring-black/5 backdrop-blur-md dark:ring-white/10";
  const cardTitle = document.createElement("div");
  cardTitle.className = "text-[15px] font-semibold leading-snug tracking-tight text-foreground";
  const cardDesc = document.createElement("div");
  cardDesc.className = "mt-1 text-[12.5px] leading-relaxed text-muted-foreground";
  card.append(cardTitle, cardDesc);
  const cardRoot = document.createElement("div");
  cardRoot.append(card);
  // 置き場所は中心を基準に自分で決める（上に入らなければ下、左右は枠の中に収める）。
  const cardMarker = new maplibre.Marker({ element: cardRoot, anchor: "center" });
  const MARGIN = 8;
  const placeCard = (e: Entry) => {
    const view = visible();
    const p = map.project([e.lng, e.lat]);
    const w = card.offsetWidth;
    const h = card.offsetHeight;
    const shown = e.placement === "none" ? "above" : e.placement;
    // 札が点の上（下）にあるときは札をよける。
    const up = shown === "above" ? GAP + TAG_H + 8 : 12;
    const down = shown === "below" ? GAP + TAG_H + 8 : 14;
    const fitsAbove = p.y - up - h >= MARGIN;
    const fitsBelow = p.y + down + h <= view.y1 - MARGIN;
    const dy = fitsAbove || !fitsBelow ? -(up + h / 2) : down + h / 2;
    // 左右は枠の中に収め、角の部品（件数・操作ボタン）に重なるならその内側へずらす。
    const clamp = (x0: number, lo: number, hi: number) => Math.min(Math.max(x0, lo), Math.max(lo, hi - w));
    let x0 = clamp(p.x - w / 2, view.x0 + MARGIN, view.x1 - MARGIN);
    const y0 = p.y + dy - h / 2;
    const [tl, tr] = cornerBoxes(view);
    const hits = (b: Box | undefined) => !!b && overlaps(b, { x0, x1: x0 + w, y0, y1: y0 + h }, MARGIN);
    if (hits(tr)) x0 = clamp(x0, view.x0 + MARGIN, (tr?.x0 ?? view.x1) - MARGIN);
    if (hits(tl)) x0 = clamp(x0, (tl?.x1 ?? view.x0) + MARGIN, view.x1 - MARGIN);
    cardMarker.setOffset([x0 - (p.x - w / 2), dy]);
  };
  follow.card = () => {
    const e = active === null ? undefined : entries[active];
    if (e && cardRoot.isConnected) placeCard(e);
  };

  return {
    setActive: (i) => {
      const prev = active;
      active = i;
      if (prev !== null) style(prev);
      const mk = i === null ? undefined : figure.markers[i];
      const e = i === null ? undefined : entries[i];
      if (i === null || !mk || !e) {
        cardMarker.remove();
        return;
      }
      style(i);
      cardTitle.textContent = mk.label;
      cardDesc.textContent = mk.desc ?? "";
      cardDesc.hidden = !mk.desc;
      cardMarker.setLngLat([mk.lng, mk.lat]).addTo(map);
      cardRoot.style.zIndex = "1001";
      // 大きさは文字を入れて地図に載せてから測る。
      placeCard(e);
    },
    setCorners: (c) => {
      corners = c;
      schedule();
    },
    setInsets: (next) => {
      if (next.left === insets.left && next.bottom === insets.bottom) return;
      insets = next;
      schedule();
    },
    remove: () => {
      if (frame) cancelAnimationFrame(frame);
      map.off("render", schedule);
      cardMarker.remove();
      entries.forEach((e) => e.marker.remove());
    },
  };
}
