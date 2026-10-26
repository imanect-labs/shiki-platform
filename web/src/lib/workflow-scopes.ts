/// ワークフローのスコープ（権限の天井）の日本語の見出し。同意ダイアログとエディタで同じ文言を使う。
/// スコープの語彙そのものはサーバ（`vocab::required_scope`）が正で、ここは表示だけ（知らないスコープは名前のまま）。
export const SCOPE_LABELS: Record<string, string> = {
  "data.read": "業務データを読む",
  "data.write": "業務データを書く",
  "storage.read": "ファイルを読む",
  "storage.write": "ファイルを保存する",
  "csv.read": "CSV を読む",
  "csv.write": "CSV を書く",
  "rag.query": "社内ドキュメントを検索する",
  "http.egress": "外部サービスへ送信する",
  "workflow.start": "別のワークフローを起動する",
  "notify.send": "人へのお知らせ（承認の依頼を含む）",
};

export function scopeLabel(scope: string): string {
  return SCOPE_LABELS[scope] ?? scope;
}
