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
/// 取り直しを始める経過時間。TTL より少し前にして、表示中の値が切れる前に差し替える。
const REFRESH_MS = TTL_MS - 5_000;

/// 名前と一緒に取れるノードの付帯情報（引用カードの「置き場所・更新日」・原本リンク用）。
export type NodeMeta = {
  name: string;
  parentId: string | null;
  updatedAt: string | null;
  contentType: string | null;
};

/// 解決済み（value=null は解決不能として確定）。`at` は解決時刻（TTL 判定）。
const resolved = new Map<string, { name: string | null; meta: NodeMeta | null; at: number }>();
/// 進行中のリクエスト（同一 id の重複発行を防ぐ）。
const inflight = new Map<string, Promise<string | null>>();

function fetchName(id: string): Promise<string | null> {
  const existing = inflight.get(id);
  if (existing) return existing;
  const p = getNode(id)
    .then((node) => {
      const name = typeof node?.name === "string" && node.name.trim() ? node.name.trim() : null;
      const meta: NodeMeta | null = name
        ? {
            name,
            parentId: node.parent_id ?? null,
            updatedAt: node.updated_at ?? null,
            contentType: node.content_type ?? null,
          }
        : null;
      resolved.set(id, { name, meta, at: Date.now() });
      return name;
    })
    .catch(() => {
      // 404/403 も含めて「解決不能」として確定させる（TTL 内は再試行しない）。
      resolved.set(id, { name: null, meta: null, at: Date.now() });
      return null;
    })
    .finally(() => {
      inflight.delete(id);
    });
  inflight.set(id, p);
  return p;
}

/// 与えた node_id 群を解決し、`pick` で取り出した値の `id → value` マップを返す（未解決の id は含まれない）。
///
/// 依存は id の集合であって配列の同一性ではないため、キーを結合した文字列で比較する
/// （毎レンダー新しい配列が来ても再取得しない）。
function useResolved<T>(
  ids: readonly string[],
  pick: (hit: { name: string | null; meta: NodeMeta | null }) => T | null,
): Record<string, T> {
  const key = React.useMemo(() => Array.from(new Set(ids)).sort().join(","), [ids]);
  // 解決完了・期限前の取り直しのたびに進める。memo はこれを依存に持つ（既存エントリの上書きは
  // Map の size を変えないため、size を依存にすると取り直した値が反映されない）。
  const [tick, bump] = React.useReducer((n: number) => n + 1, 0);

  React.useEffect(() => {
    if (!key) return;
    let active = true;
    const all = key.split(",").filter(Boolean);
    const pending = all.filter(isStale);
    if (pending.length > 0) {
      void Promise.all(pending.map(fetchName)).then(() => {
        // 解決後に一度だけ再描画する（1 件ごとに揺らさない）。
        if (active) bump();
      });
    }
    // 表示中の値は TTL で消えるので、その少し前に取り直す（マウントしたまま期限を迎えても
    // 「読み込み中」に戻らないように）。
    // 取得中（stale）の id は解決時の bump で回るので、ここでは新しいものだけを見る
    // （取得中を含めると期限切れの時刻で即時タイマーが回り続ける）。
    const refreshAt = Math.min(
      ...all.filter((id) => !isStale(id)).map((id) => resolved.get(id)!.at + REFRESH_MS),
    );
    const timer = Number.isFinite(refreshAt)
      ? window.setTimeout(() => active && bump(), Math.max(0, refreshAt - Date.now()))
      : null;
    return () => {
      active = false;
      if (timer != null) window.clearTimeout(timer);
    };
  }, [key, tick]);

  return React.useMemo(() => {
    const out: Record<string, T> = {};
    for (const id of key ? key.split(",") : []) {
      const hit = resolved.get(id);
      // TTL 切れの値は返さない（権限失効後にファイル名と存在を開示し続けない）。
      if (!hit || Date.now() - hit.at >= TTL_MS) continue;
      const v = pick(hit);
      if (v != null) out[id] = v;
    }
    return out;
    // key が変わるか、解決完了・取り直しで tick が進んだときだけ作り直す。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, tick]);
}

/// 与えた node_id 群の名前を解決し、`id → name` のマップを返す（未解決の id は含まれない）。
export function useNodeNames(ids: readonly string[]): Record<string, string> {
  return useResolved(ids, (hit) => hit.name);
}

/// 与えた node_id 群の付帯情報（名前・親フォルダ・更新日）を解決する（未解決の id は含まれない）。
export function useNodeMetas(ids: readonly string[]): Record<string, NodeMeta> {
  return useResolved(ids, (hit) => hit.meta);
}

/// 解決を試みて名前が取れなかった（削除済み・権限なし）と確定しているか。読み込み中は false。
export function isUnresolvable(id: string): boolean {
  const hit = resolved.get(id);
  return !!hit && hit.name === null && Date.now() - hit.at < TTL_MS;
}

/// 取り直し時刻を過ぎた・未解決なら再取得が要る。
function isStale(id: string): boolean {
  const hit = resolved.get(id);
  return !hit || Date.now() - hit.at >= REFRESH_MS;
}

/// テスト用: キャッシュを空にする。
export function __resetNodeNameCache(): void {
  resolved.clear();
  inflight.clear();
}
