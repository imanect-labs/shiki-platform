import type { components } from "@/generated/api";
import { apiFetch } from "@/lib/api";

/// OpenAPI(utoipa) から生成した型（手書きの対応物を作らない）。
export type MapConfig = components["schemas"]["MapConfigResponse"];

const DISABLED: MapConfig = { style_url_light: null, style_url_dark: null };

let cached: Promise<MapConfig> | null = null;

/// 地図の背景タイルの設定。会話に地図がいくつあっても 1 回だけ読む（サーバーの設定なので変わらない）。
/// 読めない（未ログイン・古いサーバー）ときは無効として扱い、Yui の静止画を描く。失敗は次の地図で読み直す。
export function fetchMapConfig(): Promise<MapConfig> {
  cached ??= apiFetch("/map/config")
    .then(async (res) => (res.ok ? ((await res.json()) as MapConfig) : DISABLED))
    .catch(() => DISABLED)
    .then((c) => {
      if (c === DISABLED) cached = null;
      return c;
    });
  return cached;
}
