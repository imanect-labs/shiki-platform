/// 文書内検索の一致を塗る（CSS Custom Highlight API）。
///
/// テキスト層の DOM を書き換えずに Range だけを登録するので、文字の選択やコピーを壊さない。
/// ページごとに自分の Range を登録・解除し、ここで全ページぶんを 2 つの Highlight
/// （一致すべて／今の一致）にまとめる。API の無いブラウザでは塗らない（移動はできる）。

const ALL = "pdf-find";
const ACTIVE = "pdf-find-active";

const byPage = new Map<string, { all: Range[]; active: Range | null }>();

type HighlightRegistry = Map<string, unknown>;
type HighlightCtor = new (...ranges: Range[]) => unknown;

function registry(): { reg: HighlightRegistry; Ctor: HighlightCtor } | null {
  if (typeof window === "undefined") return null;
  const reg = (CSS as unknown as { highlights?: HighlightRegistry }).highlights;
  const Ctor = (window as unknown as { Highlight?: HighlightCtor }).Highlight;
  return reg && Ctor ? { reg, Ctor } : null;
}

function flush() {
  const api = registry();
  if (!api) return;
  const all: Range[] = [];
  const active: Range[] = [];
  for (const v of byPage.values()) {
    all.push(...v.all);
    if (v.active) active.push(v.active);
  }
  api.reg.set(ALL, new api.Ctor(...all));
  api.reg.set(ACTIVE, new api.Ctor(...active));
}

/// ページ `key` の一致を登録する（空で呼ぶと解除）。
export function setPageHighlights(key: string, all: Range[], active: Range | null): void {
  if (all.length === 0 && !active) byPage.delete(key);
  else byPage.set(key, { all, active });
  flush();
}
