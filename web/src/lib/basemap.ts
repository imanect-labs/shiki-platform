// 背景地図（shiki-server が配る PMTiles）の設定とスタイルを読む（design.md §4.7）。
//
// ブラウザは shiki 以外へ接続しない。スタイル・地図データとも同一オリジンの BFF（/api/map/*）から読み、
// スタイルは glyphs・sprite を持たない（文字は端末のフォントで描く）。
import type { components } from "@/generated/api";
import { apiFetch, apiUrl } from "@/lib/api";

/// OpenAPI(utoipa) から生成した型。
export type MapConfigResponse = components["schemas"]["MapConfigResponse"];
export type MapAttribution = components["schemas"]["MapAttribution"];

/// 読み込んだ背景地図の設定（地図データが無ければ null）。
export type BasemapConfig = {
  /// スタイル JSON の場所（API の根からの相対パス）。
  styleUrl: string;
  /// 地図に必ず出す出典。
  attribution: MapAttribution[];
};

let configPromise: Promise<BasemapConfig | null> | null = null;
/// 読み終えた設定（まだなら undefined）。
let resolved: BasemapConfig | null | undefined;

/// 読み終えた設定をすぐ返す（まだ読んでいなければ undefined）。
export function peekBasemapConfig(): BasemapConfig | null | undefined {
  return resolved;
}

/// 背景地図の設定を読む（ページ内で 1 回だけ。失敗したら次の呼び出しで読み直す）。
/// 地図データが無い・読めない・未ログインのときは null（呼び出し側は背景なしの図を描く）。
export function loadBasemapConfig(): Promise<BasemapConfig | null> {
  if (!configPromise) {
    configPromise = (async () => {
      const res = await apiFetch("/map/config");
      // 未ログイン・権限なしは読み直しても変わらないので、このページでは背景なしと決める
      // （地図のたびに同じ要求を繰り返さない）。
      if (res.status === 401 || res.status === 403) {
        resolved = null;
        return null;
      }
      if (!res.ok) throw new Error(`/map/config が ${res.status} を返しました`);
      const body = (await res.json()) as MapConfigResponse;
      resolved = body.available && body.style_url ? { styleUrl: body.style_url, attribution: body.attribution } : null;
      return resolved;
    })().catch(() => {
      // 一時的な失敗で以後ずっと背景なしにならないよう、次は読み直す。
      configPromise = null;
      return null;
    });
  }
  return configPromise;
}

/// スタイルの中の `pmtiles://` の後ろが API の根からの相対パス（`/map/basemap.pmtiles`）なら、
/// 自分のオリジンの BFF の絶対 URL に読み替える。それ以外（外部の URL など）は受け付けない。
export function resolveArchiveUrl(url: string, origin: string): string | null {
  const prefix = "pmtiles://";
  if (!url.startsWith(prefix)) return null;
  const path = url.slice(prefix.length);
  if (!path.startsWith("/") || path.startsWith("//")) return null;
  return new URL(apiUrl(path), origin).href;
}

/// MapLibre のスタイル（必要な部分だけの型）。
export type BasemapStyle = {
  version: number;
  sources: Record<string, { type: string; url?: string; [k: string]: unknown }>;
  layers: unknown[];
  glyphs?: string;
  sprite?: unknown;
  [k: string]: unknown;
};

/// スタイル JSON を読み、地図データの URL を自分のオリジンの絶対 URL に直して返す。
/// 外部へ接続し得るもの（glyphs・sprite・自分以外を指すソース）が含まれていたら取り除く
/// （サーバのスタイルは持たない前提だが、ブラウザ側でも外へ出ないことを保つ）。
export async function loadBasemapStyle(
  styleUrl: string,
  theme: "light" | "dark",
  origin: string,
): Promise<{ style: BasemapStyle; archives: string[] }> {
  const res = await apiFetch(`${styleUrl}?theme=${theme}`);
  if (!res.ok) throw new Error(`${styleUrl} が ${res.status} を返しました`);
  const style = (await res.json()) as BasemapStyle;
  delete style.glyphs;
  delete style.sprite;
  const archives: string[] = [];
  const sources: BasemapStyle["sources"] = {};
  for (const [id, src] of Object.entries(style.sources ?? {})) {
    const archive = typeof src.url === "string" ? resolveArchiveUrl(src.url, origin) : null;
    if (!archive) continue;
    archives.push(archive);
    sources[id] = { ...src, url: `pmtiles://${archive}` };
  }
  const kept = new Set(Object.keys(sources));
  style.sources = sources;
  style.layers = (style.layers ?? []).filter((l) => {
    const src = (l as { source?: unknown }).source;
    return src === undefined || (typeof src === "string" && kept.has(src));
  });
  return { style, archives };
}
