/// CSV 文字列のパース/シリアライズ（RFC4180・下書き CSV のローカルグリッド用・Task 11.11）。
///
/// 下書きは**クライアント内のみ**のデータなのでここで往復する（実体化後の CSV は
/// サーバの TabularService（隔離 DuckDB）が正）。クォート（`"a,b"`）・エスケープ（`""`）・
/// CRLF/LF 混在を受け付け、シリアライズは `tableToCsv`（tabular-api）と同じ規則で書く。

/// CSV をセル行列へパースする（クォート対応・空行はスキップ）。
export function parseCsv(text: string): string[][] {
  const rows: string[][] = [];
  let row: string[] = [];
  let cell = "";
  let inQuotes = false;
  // 行/セルを 1 パスで読む（クォート内の改行・カンマはセルの一部）。
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (inQuotes) {
      if (ch === '"') {
        if (text[i + 1] === '"') {
          cell += '"';
          i++;
        } else {
          inQuotes = false;
        }
      } else {
        cell += ch;
      }
      continue;
    }
    if (ch === '"') {
      inQuotes = true;
    } else if (ch === ",") {
      row.push(cell);
      cell = "";
    } else if (ch === "\n" || ch === "\r") {
      if (ch === "\r" && text[i + 1] === "\n") i++;
      row.push(cell);
      cell = "";
      // 完全な空行（セル 1 個かつ空）はスキップする。
      if (row.length > 1 || row[0] !== "") rows.push(row);
      row = [];
    } else {
      cell += ch;
    }
  }
  if (cell !== "" || row.length > 0) {
    row.push(cell);
    if (row.length > 1 || row[0] !== "") rows.push(row);
  }
  return rows;
}

/// セル行列を CSV 文字列へ（RFC4180 準拠のクォート・末尾改行あり）。
export function toCsv(rows: string[][]): string {
  const esc = (s: string) => (/[",\n\r]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s);
  return rows.map((r) => r.map(esc).join(",")).join("\n") + "\n";
}
