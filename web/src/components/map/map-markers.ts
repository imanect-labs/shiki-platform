// 地図の上の地点（位置の赤いピンと名前の札）と、地点の詳しい内容のカード（MapLibre の Marker・DOM で描く）。
//
// - 地点には赤いピン（先端がその場所）を立て、名前は白い角丸の札でピンの上・右・左・下のうち空いている所に出す。
//   どこに置いてもほかの札・ピンと重なるときは札を出さずピンだけにする（一覧の順で先の地点を優先）。
// - 指した（マウスを乗せた・キーボードで移った）地点・選んだ地点はピンを大きくし、札を黒くして、カードを出す。
//
// 名前・説明は回答やノートの文字なので、HTML として解釈させない（textContent で入れる）。
import type { MapFigure } from "@imanect/yui";
import type { Map as MlMap, Marker } from "maplibre-gl";

/// 札の高さ。
const TAG_H = 28;
/// ピンの幅と高さ（先端が地点）と、ピンの頭（丸い所）の中心の高さ。地点が多い図は小さくして札の置き場を空ける。
type PinSize = { w: number; h: number; head: number };
const pinSize = (w: number, h: number): PinSize => ({ w, h, head: h - w / 2 });
const PIN = pinSize(22, 29);
const PIN_SMALL = pinSize(16, 21);
/// これより地点の多い図は小さいピンにする。
const SMALL_PIN_OVER = 10;
/// ピンから札までの間隔と、地点から下の札までの間隔。
const GAP = 4;
const BELOW = 6;

type Placement = "above" | "right" | "left" | "below" | "none";

/// 札を置き場所に置く（地点が原点。上はピンの上、左右はピンの頭の横、下は地点の下）。
/// 位置は translate で決め、指しているときの拡大（scale）とは別のプロパティにする。
function placeTag(el: HTMLElement, placement: Exclude<Placement, "none">, pin: PinSize) {
  const st = el.style;
  st.left = st.right = st.top = st.bottom = "";
  switch (placement) {
    case "above":
      st.left = "0";
      st.bottom = `${pin.h + GAP}px`;
      st.translate = "-50% 0";
      break;
    case "below":
      st.left = "0";
      st.top = `${BELOW}px`;
      st.translate = "-50% 0";
      break;
    case "right":
      st.left = `${pin.w / 2 + GAP}px`;
      st.top = `${-pin.head}px`;
      st.translate = "0 -50%";
      break;
    case "left":
      st.right = `${pin.w / 2 + GAP}px`;
      st.top = `${-pin.head}px`;
      st.translate = "0 -50%";
      break;
  }
}

const TAG_BASE =
  "absolute flex h-[28px] max-w-[180px] items-center whitespace-nowrap rounded-full px-3 text-[12.5px] font-semibold tracking-tight " +
  "leading-none shadow-[0_2px_8px_rgba(0,0,0,0.16)] ring-1 transition-[background-color,color,transform] duration-150 " +
  "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring";
const TAG_IDLE = "bg-background text-foreground ring-black/5 hover:scale-[1.04] dark:ring-white/10";
const TAG_ACTIVE = "scale-[1.06] bg-foreground text-background ring-foreground";
/// ピン（先端を地点に合わせる。指しているときは先端を軸に大きくする）。
const PIN_BASE =
  "absolute left-0 top-0 block -translate-x-1/2 -translate-y-full origin-bottom cursor-pointer " +
  "transition-transform duration-150 [filter:drop-shadow(0_2px_3px_rgba(0,0,0,0.35))]";
/// ピンの形（しずく形に白い縁と白い丸。中身は固定の図形だけで、地点の文字は入れない）。
const PIN_SVG =
  '<svg viewBox="0 0 26 34" width="100%" height="100%" aria-hidden="true">' +
  '<path d="M13 33.2c-.5 0-1-.3-1.3-.7C9 28.4 1.2 20.6 1.2 13 1.2 6.5 6.5 1.2 13 1.2S24.8 6.5 24.8 13c0 7.6-7.8 15.4-10.5 19.5-.3.4-.8.7-1.3.7z" ' +
  'fill="#e5484d" stroke="#ffffff" stroke-width="1.6"/><circle cx="13" cy="13" r="4.6" fill="#ffffff"/></svg>';

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
  pin: HTMLSpanElement;
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

function tagBox(placement: Exclude<Placement, "none">, x: number, y: number, w: number, pin: PinSize): Box {
  switch (placement) {
    case "above":
      return { x0: x - w / 2, x1: x + w / 2, y0: y - pin.h - GAP - TAG_H, y1: y - pin.h - GAP };
    case "below":
      return { x0: x - w / 2, x1: x + w / 2, y0: y + BELOW, y1: y + BELOW + TAG_H };
    case "right":
      return { x0: x + pin.w / 2 + GAP, x1: x + pin.w / 2 + GAP + w, y0: y - pin.head - TAG_H / 2, y1: y - pin.head + TAG_H / 2 };
    case "left":
      return { x0: x - pin.w / 2 - GAP - w, x1: x - pin.w / 2 - GAP, y0: y - pin.head - TAG_H / 2, y1: y - pin.head + TAG_H / 2 };
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
  const pinDim = figure.markers.length > SMALL_PIN_OVER ? PIN_SMALL : PIN;
  const entries: Entry[] = figure.markers.map((mk, i) => {
    // MapLibre に渡す要素（大きさ 0 の点）。位置は MapLibre が class と transform で決めるので触らない。
    const root = document.createElement("div");
    root.addEventListener("mouseenter", () => handlers.onHover(i));
    root.addEventListener("mouseleave", () => handlers.onLeave(i));
    const pin = document.createElement("span");
    pin.setAttribute("aria-hidden", "true");
    pin.innerHTML = PIN_SVG;
    pin.style.width = `${pinDim.w}px`;
    pin.style.height = `${pinDim.h}px`;
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
    pin.addEventListener("click", select);
    const text = document.createElement("span");
    text.className = "truncate";
    text.textContent = mk.label;
    tag.append(text);
    root.append(pin, tag);
    const marker = new maplibre.Marker({ element: root, anchor: "center" })
      .setLngLat([mk.lng, mk.lat])
      .addTo(map);
    return { marker, root, tag, pin, lng: mk.lng, lat: mk.lat, width: 0, placement: "above" };
  });

  const style = (i: number) => {
    const e = entries[i];
    if (!e) return;
    const on = active === i;
    // 札を出せない地点も、指している間はピンの上に出す。
    const placement: Placement = e.placement === "none" && on ? "above" : e.placement;
    e.tag.className =
      placement === "none" ? "hidden" : `${TAG_BASE} ${on ? TAG_ACTIVE : TAG_IDLE}`;
    if (placement !== "none") placeTag(e.tag, placement, pinDim);
    e.pin.className = `${PIN_BASE} ${on ? "scale-[1.25]" : ""}`;
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
    // ピンはすべて障害物（札でピンを隠さない）。
    const pins: Box[] = pts.map((p) => ({ x0: p.x - pinDim.w / 2, x1: p.x + pinDim.w / 2, y0: p.y - pinDim.h, y1: p.y + 1 }));
    const tags: Box[] = [];
    entries.forEach((e, i) => {
      const p = pts[i];
      if (!p) return;
      const inView = p.x >= view.x0 && p.x <= view.x1 && p.y >= view.y0 && p.y <= view.y1;
      let next: Placement = "none";
      if (inView) {
        for (const c of ["above", "right", "left", "below"] as const) {
          const box = tagBox(c, p.x, p.y, e.width, pinDim);
          const inside = box.x0 >= view.x0 && box.x1 <= view.x1 && box.y0 >= view.y0 && box.y1 <= view.y1;
          const free =
            !tags.some((b) => overlaps(b, box)) &&
            !pins.some((b, j) => j !== i && overlaps(b, box, 1)) &&
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
    // ピンと、その上（下）の札をよける。
    // 地点からカードの下端・上端までの距離（ピンと、その上・下の札をよける。指している間のピンは大きい）。
    const up = shown === "above" ? pinDim.h * 1.25 + GAP + TAG_H + 8 : pinDim.h * 1.25 + 8;
    const down = shown === "below" ? BELOW + TAG_H + 8 : 12;
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
