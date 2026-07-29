/// コンポーザのスラッシュコマンド（issue #387）。
///
/// **コマンド定義はフロントに持たない。** 候補はすべて `GET /skills/catalog`（＝モデルが
/// `skill` ツールで見ているカタログと同一の源）から来る。別の一覧をフロントで組み立てると
/// 「補完に出たのに呼べない」ずれが生まれる。
///
/// コマンドは**起動の入口を作るだけ**で、能力は一切増やさない。送信本文は
/// `/deep-research auto <依頼>` という**リテラル**にし、意味づけは skill の instructions が持つ
/// （フロントに指示文を書かない＝ skill が唯一の正）。

import { apiFetch } from "@/lib/api";
import type { SkillCommand } from "@/generated/gui-spec";

/// カタログ 1 件（`GET /skills/catalog`）。
export type SkillCatalogItem = {
  id: string;
  version: number;
  name: string;
  description: string;
  command?: SkillCommand | null;
};

/// 補完候補 1 件（コマンド × 引数プリセットの直積）。
///
/// **`token` は一意ではない**: 別々のスキルが同じコマンド名を宣言できる（名前空間を
/// 強制すると first-party とテナント自作が衝突したときに片方を使えなくする）。
/// リストのキーには `key`（skill 単位で一意）を使い、どのスキルかは `skillName` で示す。
export type SlashSuggestion = {
  /// リスト描画用の一意キー（`<skillId>:<args>`）。
  key: string;
  /// `/` の後ろに入る全文（`deep-research auto`）。
  token: string;
  /// コマンド名（`deep-research`）。
  command: string;
  /// 引数（`auto`・無しは空文字）。
  args: string;
  /// 表示名（スキル名）。
  skillName: string;
  skillId: string;
  skillVersion: number;
  /// 候補の説明（variant があればその summary、無ければ skill の description）。
  summary: string;
  hint: string | null;
};

/// 確定したコマンド（コンポーザがピルとして持つ）。
export type ActiveCommand = {
  token: string;
  command: string;
  args: string;
  skillName: string;
  skillId: string;
  skillVersion: number;
  hint: string | null;
};

export async function fetchSkillCatalog(): Promise<SkillCatalogItem[]> {
  const res = await apiFetch("/skills/catalog");
  if (!res.ok) throw new Error(`API ${res.status}`);
  const data = (await res.json()) as { skills?: SkillCatalogItem[] };
  return data.skills ?? [];
}

/// カタログを補完候補へ展開する。コマンド宣言の無いスキルは候補にしない
/// （＝コマンドで呼べないものを一覧に出さない）。
export function toSuggestions(items: SkillCatalogItem[]): SlashSuggestion[] {
  const out: SlashSuggestion[] = [];
  for (const item of items) {
    const cmd = item.command;
    if (!cmd?.name) continue;
    // variants が空なら「引数なし」の 1 候補として扱う。
    const variants = cmd.variants.length > 0 ? cmd.variants : [{ args: "", summary: item.description }];
    for (const v of variants) {
      const args = v.args.trim();
      out.push({
        key: `${item.id}:${args}`,
        token: args ? `${cmd.name} ${args}` : cmd.name,
        command: cmd.name,
        args,
        skillName: item.name,
        skillId: item.id,
        skillVersion: item.version,
        summary: v.summary.trim() || item.description,
        hint: cmd.hint ?? null,
      });
    }
  }
  return out;
}

/// 入力欄の先頭がコマンド入力中かを判定し、絞り込み済みの候補を返す。
///
/// 補完を出すのは**先頭が `/` のときだけ**（文中の `/` は URL・パスなので拾わない）。
/// 空白を含んだ時点で「引数を打っている」とみなし、コマンド名部分で絞り込む。
export function matchSuggestions(
  value: string,
  suggestions: SlashSuggestion[],
): SlashSuggestion[] | null {
  if (!value.startsWith("/")) return null;
  const typed = value.slice(1);
  // 改行が入ったら本文の一部（コマンドではない）。
  if (typed.includes("\n")) return null;
  const lower = typed.toLowerCase();
  const hits = suggestions.filter((s) => s.token.toLowerCase().startsWith(lower));
  // 完全一致 1 件だけになっても候補は出し続ける（Enter で確定できることを示す）。
  return hits;
}

/// 送信本文を組み立てる。コマンドはリテラルとして本文の先頭に置く
/// （モデルは skill の instructions でこのリテラルを解釈する）。
export function composeText(command: ActiveCommand | null, body: string): string {
  const text = body.trim();
  if (!command) return text;
  return text ? `/${command.token} ${text}` : `/${command.token}`;
}

/// 本文の先頭にあるコマンドリテラルを取り出す（トランスクリプトのチップ表示用）。
/// 候補の照合はしない（過去の発話は当時のカタログに依存するため）。
export function splitLeadingCommand(text: string): { token: string; rest: string } | null {
  if (!text.startsWith("/")) return null;
  const firstLine = text.split("\n", 1)[0];
  const m = /^\/([a-z0-9][a-z0-9-]*(?: [^\s]+)?)/.exec(firstLine);
  if (!m) return null;
  return { token: m[1], rest: text.slice(m[0].length).trimStart() };
}
