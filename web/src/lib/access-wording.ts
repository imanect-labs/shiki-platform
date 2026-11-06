/// 使う人の権限の確かめ（GAP-37）の文言。
///
/// 足りるかの判定はサーバー（`gui::access`）が決め、ここは結果を言葉にするだけ（同じ決まりを画面に持たない）。
/// 対象の名前はサーバーが返したもの（求めた人に見えるものだけ）を使い、返らなければ種類だけで言う。

import type {
  AccessNeedView,
  AccessPart,
  MyAccessItem,
  MyPartAccess,
  UncheckedBlock,
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

/// 要る権限 1 つの、対象と操作（名前はサーバーが返したもの・あなたに見える対象だけ）。作る人の確かめと、部品の版を
/// 上げる差（GAP-11）で同じ言い方にする。
export function needWhat(n: Pick<AccessNeedView, "object" | "need" | "target" | "dynamic" | "over_limit">): string {
  return n.dynamic
    ? `実行のときに決まる${needPhrase(n.object, n.need)}`
    : n.target
      ? needPhrase(n.object, n.need, n.target.name)
      : n.over_limit
        ? `${needPhrase(n.object, n.need)}（対象が多いため確かめていません）`
        : `あなたが見られないか、削除された${needPhrase(n.object, n.need)}`;
}

/// 作る人向けに、要る権限 1 つを言う（部品と、対象に要る操作）。場合によって要るかは、呼ぶ側が
/// [`CONDITIONAL_MARK`] の印で添える。
export function needLine(n: AccessNeedView): string {
  const part = partPhrase(n.part);
  const what = needWhat(n);
  return part ? `${part}: ${what}` : what;
}

/// 道によって通るブロックか、失敗を「エラー時」の出口で扱うブロックだけが要る権限に添える印（BUG-216）。
export const CONDITIONAL_MARK = "場合によって要る";

/// 場合によって要る権限の説明（作る人の共有の画面・BUG-216）。毎回は通らないブロック（分かれた流れ・「エラー時」の
/// 出口の先・繰り返しの中）と、失敗を「エラー時」の出口で扱うブロック（`on_error: continue`）の 2 つを言う。
export function conditionalNote(subject: "app" | "workflow"): string {
  const then = subject === "app" ? "足りなくてもボタンは押せますが" : "足りなくても実行できますが";
  return `${CONDITIONAL_MARK}権限は、毎回は通らないブロックと、失敗を「エラー時」の出口で扱うブロックが使います。${then}、そのブロックまで進むと失敗します。`;
}

/// 前もって確かめられないブロックの理由（GAP-82）。
function uncheckedReason(kind: UncheckedBlock["kind"]): string {
  switch (kind) {
    case "script":
      return "スクリプトの中で使うテーブルやファイルは、動くまで決まりません。";
    case "agent_tools":
      return "AI エージェントは、使う人が見られる文書やファイルだけを読みます。";
    case "secret":
      return "使う人がシークレットを使えるかは、確かめていません。";
    case "start_workflow":
      return "実行する別のワークフローと、それに要る権限は確かめていません。";
    case "drive_top":
      return "ドライブのトップに保存できるのは、組織のメンバーだけです。";
  }
}

/// 作る人向けに、前もって確かめられないブロック 1 つを言う（部品とブロックの名前と理由・GAP-82）。
export function uncheckedLine(u: UncheckedBlock): string {
  const part = partPhrase(u.part);
  return `${part ? `${part}の` : ""}「${u.block}」: ${uncheckedReason(u.kind)}`;
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

/// ワークフローを手動で実行する人に、実行できない理由を言う（アプリのボタンと同じ決まり・GAP-37）。足りていれば null。
/// 頼む先は、押した人がワークフローの持ち主かで分ける（付録 C の P4）: 持ち主でなければワークフローを作成した人、
/// 持ち主なら、対象の持ち主に共有を頼むか、ブロックの設定で選び直す。
export function myRunBlockedReason(items: readonly MyAccessItem[], viewerIsOwner = false): string | null {
  const missing = items.filter((i) => i.status === "missing");
  if (missing.length === 0) return null;
  const first = myItem(missing[0], "このワークフローが使う");
  const more = missing.length > 1 ? `ほかに ${missing.length - 1} 件、足りない権限があります。` : "";
  const ask = viewerIsOwner
    ? `${NOUN[missing[0].object]}の持ち主に共有を頼むか、ブロックの設定で選び直してください。`
    : "ワークフローを作成した人に頼んでください。";
  return `${first}${more}${ask}`;
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
