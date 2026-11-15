import { AD_GROUPS, DIRECTORY_ROLE_INDEX, type Mapping } from "./mock-directory-model";

export type ImportResult = { mapping: Mapping; errors: string[]; rows: number };

/** RFC 4180の引用符・改行・BOMに対応する小さなCSVリーダー。 */
function csvRows(text: string): string[][] {
  const rows: string[][] = [];
  let row: string[] = [], field = "", quoted = false, closed = false;
  const input = text.replace(/^\uFEFF/, "").replace(/\r\n/g, "\n");
  for (let i = 0; i < input.length; i++) {
    const c = input[i];
    if (quoted) {
      if (c === '"' && input[i + 1] === '"') { field += '"'; i++; }
      else if (c === '"') { quoted = false; closed = true; }
      else field += c;
    } else if (c === "," || c === "\n") {
      row.push(field.trim()); field = ""; closed = false;
      if (c === "\n") { rows.push(row); row = []; }
    } else if (c === '"' && field === "" && !closed) quoted = true;
    else if (closed || c === '"') throw new Error("CSVの引用符が不正です。");
    else field += c;
  }
  if (quoted) throw new Error("CSVの引用符が閉じていません。");
  if (field || row.length || closed) rows.push([...row, field.trim()]);
  return rows.filter((r) => r.some(Boolean));
}

/** 対象グループの対応付けを置換する案を作る。元の編集内容は変更しない。 */
export function importMapping(text: string, current: Mapping, roleIndex: ReadonlyMap<string, unknown> = DIRECTORY_ROLE_INDEX): ImportResult {
  const errors: string[] = [];
  let rows: string[][];
  try { rows = csvRows(text); }
  catch (e) { return { mapping: current, errors: [(e as Error).message], rows: 0 }; }
  const header = rows.shift();
  if (header?.join(",") !== "group_id,role_id") return { mapping: current, errors: ["ヘッダーは group_id,role_id にしてください。"], rows: 0 };
  if (!rows.length || rows.length > 1000) return { mapping: current, errors: ["データは1〜1000行で指定してください。"], rows: rows.length };
  const imported: Mapping = {};
  rows.forEach((row, i) => {
    const line = `データ${i + 1}行目`;
    if (row.length !== 2) { errors.push(`${line}: 列は2つ必要です。`); return; }
    const [group, role] = row;
    if (!AD_GROUPS.some((g) => g.id === group)) errors.push(`${line}: グループID「${group}」が見つかりません。`);
    if (role && !roleIndex.has(role)) errors.push(`${line}: ロールID「${role}」が見つかりません。`);
    if (Object.hasOwn(imported, group) && (!role || imported[group].length === 0 || imported[group].includes(role))) errors.push(`${line}: 対応が重複しているか、解除と付与が混在しています。`);
    imported[group] ??= [];
    if (role) imported[group].push(role);
  });
  return { mapping: { ...current, ...imported }, errors, rows: rows.length };
}
export const CSV_TEMPLATE = "\uFEFFgroup_id,role_id\r\n" + AD_GROUPS.map((g, i) => `${g.id},${["sales", "sales-1", "sales-2", "dev", "planning", ""][i]}`).join("\r\n");
