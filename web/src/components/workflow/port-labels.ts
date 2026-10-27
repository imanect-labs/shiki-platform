/// 出口（ポート）の表示名（キャンバスのカード・スマホの一覧・つながりのラベル・設定パネル・実行の記録で同じ名前）。
/// 名前の無い出口（`switch` の case・判定の選択肢のキー）は出口の名前そのものを出す。

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
