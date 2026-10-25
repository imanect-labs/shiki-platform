/// 構造化データのグリッドの awareness（他ユーザーのカーソル位置・data-platform §9.5）。
///
/// 配信されるのは「誰が・どの行の・どの列を指しているか」だけで、値は含まない。表示名は同じテナントの
/// ディレクトリから引く（引けなければ「他のユーザー」）。

import { searchDirectory } from "@/lib/storage";

/// 手元に表示している他ユーザー 1 人の位置。
export type Peer = {
  principal: string;
  recordId: string;
  /// 指している列の参照名（行全体を指しているときは null）。
  field: string | null;
  /// 最後に受けた時刻（`Date.now()`）。
  seenAt: number;
};

/// サーバの awareness の有効期間（30 秒）より少し長く残し、送り直し（15 秒ごと）の揺れで点滅させない。
export const PEER_TTL_MS = 40_000;
/// 自分の位置を送り直す間隔（サーバは 30 秒より古い位置を配らない）。
export const AWARENESS_HEARTBEAT_MS = 15_000;

/// ライト・ダーク両方で見分けられる色（彩度を揃えた 6 色）。同じ人は常に同じ色。
const PEER_COLORS = ["#e5484d", "#0090ff", "#30a46c", "#f76b15", "#8e4ec6", "#12a594"];

export function peerColor(principal: string): string {
  let h = 0;
  for (let i = 0; i < principal.length; i++) h = (h * 31 + principal.charCodeAt(i)) >>> 0;
  return PEER_COLORS[h % PEER_COLORS.length];
}

/// `#rrggbb` に透過度を足す（グリッドの塗りに使う）。
export function withAlpha(hex: string, alpha: number): string {
  const a = Math.round(Math.max(0, Math.min(1, alpha)) * 255)
    .toString(16)
    .padStart(2, "0");
  return `${hex}${a}`;
}

let names: Promise<Map<string, string>> | null = null;

/// 同じテナントのユーザーの表示名（先頭 100 人まで・1 回だけ引く）。引けない人は呼び出し側で「他のユーザー」。
export function directoryNames(): Promise<Map<string, string>> {
  if (!names) {
    names = (async () => {
      const m = new Map<string, string>();
      let cursor: string | undefined;
      for (let i = 0; i < 2; i++) {
        const page = await searchDirectory({ q: "", cursor, limit: 50 });
        for (const u of page.items) m.set(u.id, u.display_name || u.email);
        if (!page.next_cursor) break;
        cursor = page.next_cursor;
      }
      return m;
    })().catch(() => {
      names = null;
      return new Map<string, string>();
    });
  }
  return names;
}
