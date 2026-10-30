/// テーブルの「状態の進め方」の画面（GAP-44・D-84・D-115）の下書きと検査。
///
/// サーバの定義（`FsmBody`・進められる人は行の決まりと同じ `PolicyExpr`）を、画面で編集できる形に読み替える。
///
/// - 進められる人は「テーブルを編集できる人」「レコードを追加した人」「メンバーの列の人」「部署・グループの列の人」
///   「部署・グループ」の組み合わせ（どれか 1 つに当てはまる人）で書く。これに当てはまらない条件（チャットや API で
///   書いたもの）は `custom` として、そのまま保つ（画面では変えない・保存済みの定義を壊さない）。
/// - 「申請した本人は除く」は、申請者の列（メンバーの列）と `ne $user.id` の比べを `all` で足して書く（D-115・
///   経費のテンプレート expense_policy と同じ形）。行の決まりに `not` は無いので、この形で書く。
/// - 状態は下書きの中で id で持つ（名前を変えても、進め方の行き先が付いてくる）。保存のときに名前へ戻す。

import type { FieldDef, FsmBody, PolicyExpr, TableFsmResponse } from "@/lib/data-api";

/// 進められる人の 1 つ。
export type Who =
  | { kind: "editors" }
  | { kind: "creator" }
  | { kind: "user_field"; field: string }
  | { kind: "role_field"; field: string }
  | { kind: "role"; role: string };

/// 進められる人（画面で編集できる形か、そのまま保つ条件）。
export type ActorModel =
  | { kind: "simple"; who: Who[]; excludeApplicant: boolean }
  | { kind: "custom"; expr: PolicyExpr };

export type StateDraft = { id: string; name: string };
export type MoveDraft = {
  id: string;
  from: string;
  to: string;
  actor: ActorModel;
  /// 足したばかりで「申請した本人は除く」にまだ触っていない（行き先を変えたら既定を決め直す・D-115）。
  autoExclude?: boolean;
};

export type FlowDraft = {
  statusField: string | null;
  states: StateDraft[];
  moves: MoveDraft[];
  applicantField: string | null;
  /// 最初の状態を過ぎたら、申請者は書き換えられない（D-115）。
  writeLock: boolean;
};

let seq = 0;
/// 下書きの中だけで使う id。
export function draftId(prefix: string): string {
  seq += 1;
  return `${prefix}-${seq}`;
}

const NE_SELF = (field: string): PolicyExpr => ({ field_cmp: { field, op: "ne", value: "user_id" } });

function isExclusion(e: PolicyExpr, applicant: string): boolean {
  return (
    typeof e === "object" &&
    "field_cmp" in e &&
    e.field_cmp.field === applicant &&
    e.field_cmp.op === "ne" &&
    e.field_cmp.value === "user_id"
  );
}

function baseWho(e: PolicyExpr): Who | null {
  if (e === "public") return { kind: "editors" };
  if (e === "is_owner") return { kind: "creator" };
  if (typeof e !== "object") return null;
  if ("has_role" in e) return e.has_role.subtree ? { kind: "role", role: e.has_role.role } : null;
  if ("field_cmp" in e) {
    const c = e.field_cmp;
    if (c.op === "eq" && c.value === "user_id") return { kind: "user_field", field: c.field };
    if (c.op === "in" && typeof c.value === "object" && "user_roles" in c.value && c.value.user_roles.subtree) {
      return { kind: "role_field", field: c.field };
    }
  }
  return null;
}

function whoList(e: PolicyExpr): Who[] | null {
  if (typeof e === "object" && "any" in e) {
    const xs = e.any.map(baseWho);
    return xs.length > 0 && xs.every((x) => x !== null) ? (xs as Who[]) : null;
  }
  const b = baseWho(e);
  return b ? [b] : null;
}

/// 保存済みの進められる人を、画面の形に読む（読めない条件は `custom` のまま保つ）。
export function parseActor(e: PolicyExpr, applicant: string | null): ActorModel {
  if (applicant) {
    if (isExclusion(e, applicant)) return { kind: "simple", who: [{ kind: "editors" }], excludeApplicant: true };
    if (typeof e === "object" && "all" in e && e.all.length === 2 && isExclusion(e.all[1], applicant)) {
      const w = whoList(e.all[0]);
      if (w) return { kind: "simple", who: w, excludeApplicant: true };
    }
  }
  const w = whoList(e);
  return w ? { kind: "simple", who: w, excludeApplicant: false } : { kind: "custom", expr: e };
}

function whoExpr(w: Who): PolicyExpr {
  switch (w.kind) {
    case "editors":
      return "public";
    case "creator":
      return "is_owner";
    case "user_field":
      return { field_cmp: { field: w.field, op: "eq", value: "user_id" } };
    case "role_field":
      return { field_cmp: { field: w.field, op: "in", value: { user_roles: { subtree: true } } } };
    case "role":
      return { has_role: { role: w.role, subtree: true } };
  }
}

/// 画面の形から、保存する進められる人を組む（申請者の列が無ければ「申請した本人は除く」は効かない）。
export function buildActor(a: ActorModel, applicant: string | null): PolicyExpr {
  if (a.kind === "custom") return a.expr;
  const parts = a.who.map(whoExpr);
  const who: PolicyExpr = parts.length === 1 ? parts[0] : { any: parts };
  return a.excludeApplicant && applicant ? { all: [who, NE_SELF(applicant)] } : who;
}

/// 保存済みの進め方が「申請した本人は除く」に使っている申請者の列（最初に見つかったもの）。
export function applicantFromBody(body: FsmBody, fields: FieldDef[]): string | null {
  const users = new Set(fields.filter((f) => f.type === "user_ref").map((f) => f.key));
  for (const t of body.transitions) {
    const e = t.actor;
    const tail = typeof e === "object" && "all" in e ? e.all[e.all.length - 1] : e;
    if (typeof tail === "object" && "field_cmp" in tail) {
      const c = tail.field_cmp;
      if (c.op === "ne" && c.value === "user_id" && users.has(c.field)) return c.field;
    }
  }
  return null;
}

const label = (f: FieldDef) => f.display_name || f.key;
const looks = (f: FieldDef, words: string[]) => words.some((w) => label(f).includes(w) || f.key.includes(w));

/// 申請者の列の見当（名前に「申請」・applicant などが入るメンバーの列）。見当が付かなければ選ばない。
export function guessApplicant(fields: FieldDef[]): string | null {
  return fields.find((f) => f.type === "user_ref" && looks(f, ["申請", "applicant", "requester", "submitter"]))?.key ?? null;
}

/// 新しい進め方の進められる人の見当（承認者・上長の列の人。無ければテーブルを編集できる人）。
export function guessApprover(fields: FieldDef[], applicant: string | null): Who {
  const users = fields.filter((f) => f.type === "user_ref" && f.key !== applicant);
  const hit = users.find((f) => looks(f, ["承認", "上長", "approver", "manager"]));
  return hit ? { kind: "user_field", field: hit.key } : { kind: "editors" };
}

/// 状態の列の見当（状態の進め方が無いテーブルで最初に選んでおく列）。
export function guessStatusField(fields: FieldDef[], current: string | null | undefined): string | null {
  const selects = fields.filter((f) => f.type === "select");
  if (current && selects.some((f) => f.key === current)) return current;
  return selects.find((f) => looks(f, ["状態", "status", "ステータス"]))?.key ?? null;
}

/// 列の選択肢から状態の一覧を作る。
export function statesFromField(fields: FieldDef[], key: string | null): StateDraft[] {
  const f = fields.find((x) => x.key === key);
  return (f?.options ?? []).map((name) => ({ id: draftId("s"), name }));
}

/// サーバの状態の進め方から下書きを作る。`mismatch` は列の選択肢と状態が合っていなかった（両方を合わせて出す）。
export function draftFrom(resp: TableFsmResponse, fields: FieldDef[]): { draft: FlowDraft; mismatch: boolean } {
  const fsm = resp.fsm;
  if (!fsm) {
    const statusField = guessStatusField(fields, resp.status_field);
    const applicantField = guessApplicant(fields);
    return {
      draft: { statusField, states: statesFromField(fields, statusField), moves: [], applicantField, writeLock: true },
      mismatch: false,
    };
  }
  const options = fields.find((f) => f.key === resp.status_field)?.options ?? [];
  const names = [...fsm.body.states, ...options.filter((o) => !fsm.body.states.includes(o))];
  const mismatch = names.length !== fsm.body.states.length || fsm.body.states.some((s) => !options.includes(s));
  const states = names.map((name) => ({ id: draftId("s"), name }));
  const idOf = new Map(states.map((s) => [s.name, s.id]));
  const applicantField = resp.applicant_write_lock ?? applicantFromBody(fsm.body, fields);
  const moves = fsm.body.transitions.map((t) => ({
    id: draftId("m"),
    from: idOf.get(t.from) ?? "",
    to: idOf.get(t.to) ?? "",
    actor: parseActor(t.actor, applicantField),
  }));
  return {
    draft: { statusField: resp.status_field ?? null, states, moves, applicantField, writeLock: Boolean(resp.applicant_write_lock) },
    mismatch,
  };
}

/// 下書きから保存する定義を組む。
export function bodyOf(d: FlowDraft): FsmBody {
  const name = new Map(d.states.map((s) => [s.id, s.name.trim()]));
  return {
    states: d.states.map((s) => s.name.trim()),
    transitions: d.moves.map((m) => ({
      from: name.get(m.from) ?? "",
      to: name.get(m.to) ?? "",
      actor: buildActor(m.actor, d.applicantField),
    })),
  };
}

export type FlowIssues = {
  /// 状態ごとの誤り（保存できない）。
  stateErrors: Map<string, string>;
  /// 状態ごとの注意（保存はできる）。
  stateWarnings: Map<string, string>;
  /// 進め方ごとの誤り。
  moveErrors: Map<string, string>;
  /// 全体の誤り。
  errors: string[];
  /// どの状態へも進まない状態（最後の状態）の id。
  ends: Set<string>;
  hasErrors: boolean;
};

/// 保存の前の検査（行き止まり・届かない状態・重複など）。文は欄の下に出す（付録 C の §3.4.1）。
export function checkFlow(d: FlowDraft): FlowIssues {
  const stateErrors = new Map<string, string>();
  const stateWarnings = new Map<string, string>();
  const moveErrors = new Map<string, string>();
  const errors: string[] = [];
  const seen = new Map<string, string>();
  for (const s of d.states) {
    const n = s.name.trim();
    if (!n) stateErrors.set(s.id, "状態の名前を入れてください。");
    else if ([...n].length > 128) stateErrors.set(s.id, "状態の名前は 128 字までにしてください。");
    else if (seen.has(n)) stateErrors.set(s.id, "同じ名前の状態があります。名前を変えてください。");
    else seen.set(n, s.id);
  }
  if (d.states.length === 0) errors.push("状態を 1 つ以上追加してください。");
  const ids = new Set(d.states.map((s) => s.id));
  const pairs = new Set<string>();
  for (const m of d.moves) {
    let e: string | null = null;
    if (!ids.has(m.from)) e = "いまの状態を選んでください。";
    else if (!ids.has(m.to)) e = "次の状態を選んでください。";
    else if (m.from === m.to) e = "同じ状態へは進められません。次の状態を選び直してください。";
    else if (pairs.has(`${m.from}>${m.to}`)) e = "同じ進め方がもう 1 つあります。1 つにまとめてください。";
    else if (m.actor.kind === "simple" && m.actor.who.length === 0) e = "進められる人を 1 つ以上選んでください。";
    else if (m.actor.kind === "simple" && m.actor.who.some((w) => w.kind === "role" && !w.role)) {
      e = "部署・グループを選んでください。";
    }
    pairs.add(`${m.from}>${m.to}`);
    if (e) moveErrors.set(m.id, e);
  }
  const initial = d.states[0];
  if (d.states.length > 0 && d.moves.length === 0) errors.push("進め方を 1 つ以上追加してください。");
  else if (initial && !d.moves.some((m) => m.from === initial.id)) {
    errors.push(`最初の状態「${initial.name.trim() || "（名前なし）"}」から進む先がありません。進め方を追加してください。`);
  }
  // 最初の状態からたどれる状態（届かない状態は注意だけ・使っていない選択肢のこともある）。
  const reach = reachIds(d);
  const ends = new Set<string>();
  for (const s of d.states.slice(1)) {
    if (!reach.has(s.id)) {
      stateWarnings.set(
        s.id,
        d.moves.some((m) => m.to === s.id)
          ? "最初の状態から、この状態へたどり着けません。"
          : "どの状態からも、この状態へ進めません。",
      );
    }
  }
  for (const s of d.states) if (reach.has(s.id) && !d.moves.some((m) => m.from === s.id)) ends.add(s.id);
  return {
    stateErrors,
    stateWarnings,
    moveErrors,
    errors,
    ends,
    hasErrors: stateErrors.size > 0 || moveErrors.size > 0 || errors.length > 0,
  };
}

/// 最初の状態からたどれる状態の id。
function reachIds(d: FlowDraft): Set<string> {
  const ids = new Set(d.states.map((s) => s.id));
  const reach = new Set<string>(d.states[0] ? [d.states[0].id] : []);
  for (let grew = true; grew; ) {
    grew = false;
    for (const m of d.moves) {
      if (reach.has(m.from) && ids.has(m.to) && !reach.has(m.to)) {
        reach.add(m.to);
        grew = true;
      }
    }
  }
  return reach;
}

/// 最初の状態からたどれる状態の名前（ワークフローが使う状態の注意に使う）。
export function reachableNames(d: FlowDraft): Set<string> {
  const reach = reachIds(d);
  return new Set(d.states.filter((s) => reach.has(s.id)).map((s) => s.name.trim()));
}
