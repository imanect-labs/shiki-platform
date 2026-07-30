/// storage node_id → 表示名の解決キャッシュ（issue #386）。
///
/// `office.edit` / `document.edit` / `slide.edit` / `csv.patch` などのツールは入力に
/// **node_id（UUID）しか持たない**ため、「報告書.docx を編集中」と出すには名前解決が要る。
/// SSE には名前が乗らない（ツール引数をそのまま流している）ので、クライアントで解決する。
///
/// 方針:
/// - モジュールレベルの Map に載せる（同じ会話で同じファイルを何度も編集するのが普通）。
/// - **同一 id の並行リクエストは 1 本にまとめる**（in-flight Promise を共有）。
/// - 解決できない（削除済み・権限なし）場合は `null` を記憶して再試行しない
///   （存在秘匿の観点でも、失敗を繰り返し叩かない）。
/// - 未解決の間は呼び出し側が汎用ラベルへフォールバックする（ちらつかせない）。

import * as React from "react";

import { getNode } from "@/lib/storage";

/// 解決の有効期間。共有解除・権限失効の後も名前を返し続けないための上限。
/// `getNode` は毎回サーバで認可されるため、失効の反映が遅れる窓をこの長さに限る。
const TTL_MS = 60_000;

/// 解決済み（value=null は解決不能として確定）。`at` は解決時刻（TTL 判定）。
const resolved = new Map<string, { name: string | null; at: number }>();
/// 進行中のリクエスト（同一 id の重複発行を防ぐ）。
const inflight = new Map<string, Promise<string | null>>();

function fetchName(id: string): Promise<string | null> {
  const existing = inflight.get(id);
  if (existing) return existing;
  const p = getNode(id)
    .then((node) => {
      const name = typeof node?.name === "string" && node.name.trim() ? node.name.trim() : null;
      resolved.set(id, { name, at: Date.now() });
      return name;
    })
    .catch(() => {
      // 404/403 も含めて「解決不能」として確定させる（TTL 内は再試行しない）。
      resolved.set(id, { name: null, at: Date.now() });
      return null;
    })
    .finally(() => {
      inflight.delete(id);
    });
  inflight.set(id, p);
  return p;
}

/// 与えた node_id 群の名前を解決し、`id → name` のマップを返す（未解決の id は含まれない）。
///
/// 依存は id の集合であって配列の同一性ではないため、キーを結合した文字列で比較する
/// （毎レンダー新しい配列が来ても再取得しない）。
export function useNodeNames(ids: readonly string[]): Record<string, string> {
  const key = React.useMemo(() => Array.from(new Set(ids)).sort().join(","), [ids]);
  const [, force] = React.useReducer((n: number) => n + 1, 0);

  React.useEffect(() => {
    if (!key) return;
    let active = true;
    const pending = key.split(",").filter((id) => id && isStale(id));
    if (pending.length === 0) return;
    void Promise.all(pending.map(fetchName)).then(() => {
      // 解決後に一度だけ再描画する（1 件ごとに揺らさない）。
      if (active) force();
    });
    return () => {
      active = false;
    };
  }, [key]);

  return React.useMemo(() => {
    const out: Record<string, string> = {};
    for (const id of key ? key.split(",") : []) {
      const hit = resolved.get(id);
      // TTL 切れの値は返さない（権限失効後にファイル名と存在を開示し続けない）。
      if (hit?.name && Date.now() - hit.at < TTL_MS) out[id] = hit.name;
    }
    return out;
    // key が変わるか、解決完了で force() が走ったときだけ作り直す。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, resolved.size]);
}

/// TTL 切れ・未解決なら再取得が要る。
function isStale(id: string): boolean {
  const hit = resolved.get(id);
  return !hit || Date.now() - hit.at >= TTL_MS;
}

/// テスト用: キャッシュを空にする。
export function __resetNodeNameCache(): void {
  resolved.clear();
  inflight.clear();
}
