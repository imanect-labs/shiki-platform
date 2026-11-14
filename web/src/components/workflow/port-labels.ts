/// 出口（ポート）の表示名（キャンバスのカード・スマホの一覧・つながりのラベル・設定パネル・実行の記録で同じ名前）。
/// 名前の無い出口（`switch` の case・名前の無い判定の選択肢のキー）は出口の名前そのものを出す。

const PORT_LABELS: Record<string, string> = {
  out: "次へ",
  true: "はい",
  false: "いいえ",
  default: "その他",
  error: "エラー時",
  timeout: "時間切れ",
  approved: "承認",
  rejected: "却下",
  returned: "差し戻し",
  withdrawn: "取り下げ",
  uncertain: "要確認",
};

/// 出口の表示名（決まった名前が無ければ出口の名前）。
export function portLabel(port: string): string {
  return PORT_LABELS[port] ?? port;
}

type DecideOptionLike = { key?: unknown; label?: unknown };
type DecideParamsLike = {
  route?: { question?: unknown } | null;
  questions?: { key?: unknown; options?: DecideOptionLike[] | null }[];
};

/// ノードの出口の表示名。「AI で判定」の選択肢の出口（出口の名前＝選択肢のキー・decide-renames.ts と同じ規則）は、
/// 選択肢に名前（label）があればその名前にする（D-87）。名前の無い選択肢（名前を足す前の定義）とほかの出口は
/// [`portLabel`]（今までどおりキーを名前に使う）。
export function nodePortLabel(node: { type: string; params?: unknown } | undefined, port: string): string {
  if (node?.type === "ai.decide") {
    const p = (node.params ?? {}) as DecideParamsLike;
    const q = p.questions?.find((x) => x.key === p.route?.question);
    const label = q?.options?.find((o) => o.key === port)?.label;
    if (typeof label === "string" && label.trim() !== "") return label.trim();
  }
  return portLabel(port);
}
