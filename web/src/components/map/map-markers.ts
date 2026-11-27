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
const TAG_H = 26;
const GAP = 7;

type Placement = "above" | "right" | "left" | "below" | "none";

/// 札の置き場所ごとの位置（点が原点）。
const PLACE_CLASS: Record<Exclude<Placement, "none">, string> = {
  above: "bottom-[7px] left-1/2 -translate-x-1/2",
  right: "left-[7px] top-1/2 -translate-y-1/2",
  left: "right-[7px] top-1/2 -translate-y-1/2",
  below: "top-[7px] left-1/2 -translate-x-1/2",
};

const TAG_BASE =
  "absolute flex h-[26px] max-w-[180px] items-center whitespace-nowrap rounded-full border px-2.5 text-[12.5px] font-semibold " +
  "leading-none shadow-[0_1px_4px_rgba(0,0,0,0.2)] transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring";
const TAG_IDLE = "border-border bg-background text-foreground hover:bg-secondary";
const TAG_ACTIVE = "border-foreground bg-foreground text-background";
const DOT_BASE =
  "absolute left-0 top-0 -translate-x-1/2 -translate-y-1/2 cursor-pointer rounded-full border-2 border-background shadow-[0_1px_3px_rgba(0,0,0,0.35)]";

export type MarkerHandlers = {
  /// 地点を指した・離れた（i は地点の番号・離れたら null）。
  onHover: (i: number | null) => void;
  /// 地点を押した。
  onSelect: (i: number) => void;
};

/// 地点の札とカードをまとめて扱う。
export type FigureMarkers = {
  /// 指している・選んでいる地点を強調し、カードを出す（null で消す）。
  setActive: (i: number | null) => void;
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
    root.addEventListener("mouseleave", () => handlers.onHover(null));
    const dot = document.createElement("span");
    dot.setAttribute("aria-hidden", "true");
    const tag = document.createElement("button");
    tag.type = "button";
    tag.dataset.testid = "basemap-tag";
    tag.setAttribute("aria-label", mk.desc ? `${mk.label}（${mk.desc}）` : mk.label);
    tag.addEventListener("focus", () => handlers.onHover(i));
    tag.addEventListener("blur", () => handlers.onHover(null));
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
    e.dot.className = `${DOT_BASE} ${on ? "size-3.5 bg-foreground" : "size-2.5 bg-[#d4442e]"}`;
    // 一覧の順で先の地点、指している地点を上に重ねる。
    e.root.style.zIndex = String(on ? 1000 : placement === "none" ? 1 : 500 - i);
  };

  // 札の置き場所を決める（一覧の順で先の地点から、上・右・左・下の順に空いている所へ）。
  let frame = 0;
  const layout = () => {
    frame = 0;
    const canvas = map.getCanvas();
    const view: Box = { x0: 0, y0: 0, x1: canvas.clientWidth, y1: canvas.clientHeight };
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
            !tags.some((b) => overlaps(b, box)) && !dots.some((b, j) => j !== i && overlaps(b, box, 1));
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
    "pointer-events-none w-max max-w-[260px] rounded-xl border border-border/60 bg-background px-3.5 py-2.5 text-left shadow-[0_4px_16px_rgba(0,0,0,0.16)]";
  const cardTitle = document.createElement("div");
  cardTitle.className = "text-[14px] font-semibold leading-snug text-foreground";
  const cardDesc = document.createElement("div");
  cardDesc.className = "mt-0.5 text-[12.5px] leading-snug text-muted-foreground";
  card.append(cardTitle, cardDesc);
  const cardRoot = document.createElement("div");
  cardRoot.append(card);
  const cardMarker = new maplibre.Marker({ element: cardRoot, anchor: "bottom" });

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
      // カードは点の上に出す（札が点の上にあるときは札をよける）。
      const above = e.placement === "above" || e.placement === "none";
      cardMarker
        .setOffset([0, above ? -(GAP + TAG_H + 8) : -12])
        .setLngLat([mk.lng, mk.lat])
        .addTo(map);
      cardRoot.style.zIndex = "1001";
    },
    remove: () => {
      if (frame) cancelAnimationFrame(frame);
      map.off("render", schedule);
      cardMarker.remove();
      entries.forEach((e) => e.marker.remove());
    },
  };
}
