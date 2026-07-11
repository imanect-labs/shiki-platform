/// ノート埋め込みブロックのペイロード型（Task 11P.6・3 種のみ）。
///
/// **これ以外の埋め込みは存在しない**（生 HTML/JSX はレンダリングしない・stored XSS 遮断）。
/// ペイロードは Yjs の `shikiEmbed` 要素の `payload` 属性に JSON 文字列で載り、md では
/// ```shiki-embed フェンス（JSON）へ往復する（backend の Block::Embed と対）。

export type EmbedPayload =
  /// ①genui 検証済みシキコンポーネントスペック（Phase 6 レンダラで描画）。
  | { kind: "genui"; spec: unknown }
  /// ②ミニアプリ/artifact の別オリジン iframe（sandbox＋CSP・B1 と同じ分離）。
  | { kind: "iframe"; src: string; title?: string }
  /// ③ドライブファイル参照（**閲覧者本人の ReBAC** で解決）。
  | { kind: "drive"; node_id: string; name?: string };

/// 埋め込みの種別（不明は描画しない）。
export type EmbedKind = EmbedPayload["kind"];
const KINDS: EmbedKind[] = ["genui", "iframe", "drive"];

/// JSON 文字列を検証してペイロードにする（不正・未知種別は null＝描画しない・fail-closed）。
export function parseEmbedPayload(raw: string): EmbedPayload | null {
  let value: unknown;
  try {
    value = JSON.parse(raw);
  } catch {
    return null;
  }
  if (typeof value !== "object" || value === null) return null;
  const obj = value as { kind?: unknown };
  if (typeof obj.kind !== "string" || !KINDS.includes(obj.kind as EmbedKind)) return null;

  if (obj.kind === "genui") {
    const g = value as { spec?: unknown };
    return "spec" in g ? { kind: "genui", spec: g.spec } : null;
  }
  if (obj.kind === "iframe") {
    const f = value as { src?: unknown; title?: unknown };
    // https のみ許可（javascript:/data: 等のスキームを拒否＝実行系を埋め込ませない）。
    if (typeof f.src !== "string" || !isSafeHttpUrl(f.src)) return null;
    return {
      kind: "iframe",
      src: f.src,
      title: typeof f.title === "string" ? f.title : undefined,
    };
  }
  // drive
  const d = value as { node_id?: unknown; name?: unknown };
  if (typeof d.node_id !== "string" || d.node_id.length === 0) return null;
  return {
    kind: "drive",
    node_id: d.node_id,
    name: typeof d.name === "string" ? d.name : undefined,
  };
}

/// ペイロードを正規化 JSON 文字列へ（キー順を固定して md 往復を安定させる）。
export function serializeEmbedPayload(payload: EmbedPayload): string {
  switch (payload.kind) {
    case "genui":
      return JSON.stringify({ kind: "genui", spec: payload.spec });
    case "iframe":
      return JSON.stringify({
        kind: "iframe",
        src: payload.src,
        ...(payload.title ? { title: payload.title } : {}),
      });
    case "drive":
      return JSON.stringify({
        kind: "drive",
        node_id: payload.node_id,
        ...(payload.name ? { name: payload.name } : {}),
      });
  }
}

/// http(s) の絶対 URL だけを許可する（相対・javascript:・data: 等を弾く）。
export function isSafeHttpUrl(src: string): boolean {
  try {
    const u = new URL(src);
    return u.protocol === "https:" || u.protocol === "http:";
  } catch {
    return false;
  }
}
