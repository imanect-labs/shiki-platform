/// 使う人の権限の確かめ（GAP-37）の文言。
///
/// 足りるかの判定はサーバー（`gui::access`）が決め、ここは結果を言葉にするだけ（同じ決まりを画面に持たない）。
/// 対象の名前はサーバーが返したもの（求めた人に見えるものだけ）を使い、返らなければ種類だけで言う。

import type {
  AccessNeedView,
  AccessPart,
  MyAccessItem,
  MyPartAccess,
} from "@/lib/artifact-api";

type AccessObject = AccessNeedView["object"];
type AccessKind = AccessNeedView["need"];

const NOUN: Record<AccessObject, string> = {
  table: "テーブル",
  file: "ファイル",
  folder: "フォルダー",
};

/// 対象を言う（名前が分かれば テーブル「経費」、分からなければ テーブル）。
function target(object: AccessObject, name?: string | null): string {
  return name ? `${NOUN[object]}「${name}」` : NOUN[object];
}

/// 対象に要る操作の、対象につなぐ助詞と動詞（例: に・レコードを追加する）。
function verbOf(object: AccessObject, need: AccessKind): { particle: string; verb: string } {
  if (object === "table") {
    switch (need) {
      case "read":
        return { particle: "を", verb: "見る" };
      case "add":
        return { particle: "に", verb: "レコードを追加する" };
      case "change":
        return { particle: "の", verb: "レコードを書き換える" };
      case "edit":
        return { particle: "を", verb: "編集する" };
    }
  }
  if (object === "folder") {
    return need === "read" ? { particle: "の", verb: "中を見る" } : { particle: "に", verb: "保存する" };
  }
  return need === "read" ? { particle: "を", verb: "読む" } : { particle: "を", verb: "書き換える" };
}

/// 対象に要る操作を「〜する」の形で言う（例: テーブル「経費」にレコードを追加する）。
export function needPhrase(object: AccessObject, need: AccessKind, name?: string | null): string {
  const { particle, verb } = verbOf(object, need);
  return `${target(object, name)}${particle}${verb}`;
}

/// 部品を言う（ワークフローそのものを確かめたときは null）。
export function partPhrase(part?: AccessPart | null): string | null {
  if (!part) return null;
  switch (part.kind) {
    case "workflow":
      return `ワークフロー「${part.alias}」`;
    case "data_view":
      return `表「${part.alias}」`;
    case "skill":
      return "AI に聞く（スキル）";
  }
}

/// 作る人向けに、要る権限 1 つを言う（部品と、対象に要る操作）。
export function needLine(n: AccessNeedView): string {
  const part = partPhrase(n.part);
  const what = n.dynamic
    ? `実行のときに決まる${needPhrase(n.object, n.need)}`
    : n.target
      ? needPhrase(n.object, n.need, n.target.name)
      : `あなたが見られないか、削除された${needPhrase(n.object, n.need)}`;
  return part ? `${part}: ${what}` : what;
}

/// 追加だけの共有で範囲が狭くなるときの補足。
export function limitedNote(need: AccessKind): string {
  return need === "change"
    ? "追加だけの共有なので、自分が追加したレコードのうち、状態の進め方で許したものだけ直せます。"
    : "追加だけの共有なので、自分が追加したレコードだけ見られます。";
}

/// 作る人への直し方（相手に足りない権限 1 つ）。
export function fixHint(n: AccessNeedView, subject: "user" | "role"): string {
  if (n.object === "table" && n.target?.app_id) {
    return subject === "role"
      ? "インストールしたアプリの「使える部署・グループ」に、この部署を足してください。"
      : "インストールしたアプリの「使える部署・グループ」に、この人の部署を足してください。";
  }
  const noun = NOUN[n.object];
  if (n.object === "table") {
    switch (n.need) {
      case "read":
        return "テーブルを「閲覧」で共有してください。";
      case "add":
        return "テーブルを「追加だけ」か「編集」で共有してください。";
      case "change":
      case "edit":
        return "テーブルを「編集」で共有してください。";
    }
  }
  return n.need === "read" ? `${noun}を「閲覧」で共有してください。` : `${noun}を「編集」で共有してください。`;
}

/// 使う人の足りないもの 1 つを言う。名前が返らない対象は、削除されたのか見る権限が無いのかを区別できない
/// （区別すると存在が漏れる）ので、両方を言う（付録 C の P5）。
function myItem(i: MyAccessItem, where: string): string {
  if (i.name) return `${needPhrase(i.object, i.need, i.name)}権限がありません。`;
  return `${where}${NOUN[i.object]}が見つからないか、${verbOf(i.object, i.need).verb}権限がありません。`;
}

/// 使う人に、部品を使えない理由を言う（ボタン・表）。足りていれば null。
export function myBlockedReason(p: MyPartAccess): string | null {
  const where = p.part.kind === "data_view" ? "この表が使う" : "このボタンが使う";
  if (p.unavailable) {
    return p.part.kind === "data_view"
      ? "この表のビューが見つかりません。アプリを作成した人に伝えてください。"
      : "このボタンで動くワークフローが見つかりません。アプリを作成した人に伝えてください。";
  }
  const missing = p.items.filter((i) => i.status === "missing");
  if (missing.length === 0) return null;
  const first = myItem(missing[0], where);
  const more = missing.length > 1 ? `ほかに ${missing.length - 1} 件、足りない権限があります。` : "";
  return `${first}${more}アプリを作成した人に頼んでください。`;
}
