/// アーティファクト（skill / ミニアプリ）のクライアント側データ層（Phase 6）。
///
/// backend の `/skills` `/mini-apps` `/artifacts`（共有・版）と図のボタンの実行 API を叩く。型は codegen（gui-spec.ts / api.d.ts）を正とし、
/// 手書きミラーを作らない（camelCase 変換の薄いラッパのみ）。

"use client";

import { apiFetch } from "@/lib/api";
import { ownedPosition, type FetchPage } from "@/lib/owned-shared-list";
import type { components } from "@/generated/api";
import type { MiniAppBody, SkillBody } from "@/generated/gui-spec";

export type { MiniAppBody, SkillBody };

/// 共有語彙（backend artifact::ArtifactRole / storage::ShareTarget と一致）。
export type ArtifactRole = "viewer" | "editor";
export type ShareTarget = { type: "user"; id: string } | { type: "role"; id: string };
/// 共有一覧の 1 件（表示名・email はサーバがテナント内ディレクトリから解決・#741）。
export type ArtifactShareEntry = components["schemas"]["ArtifactShareEntry"];

/// アーティファクトの種類（codegen・ワークフロー・アプリ・ビュー・スキルなど）。
export type ArtifactKind = components["schemas"]["ArtifactKind"];

export type ArtifactMeta = {
  id: string;
  kind: ArtifactKind;
  name: string;
  owner: string;
  currentVersion: number;
  /// 公開したバージョン（ミニアプリだけ・まだ公開していなければ null・D-16）。
  publishedVersion: number | null;
  createdAt: string;
  updatedAt: string;
};

export type VersionMeta = { version: number; createdBy: string; createdAt: string };

/// 保存時の検証エラー 1 件（codegen）。
type GuiValidationError = components["schemas"]["GuiValidationError"];

/// 状態コードの既定の文言（サーバーが文言を返さないときの最後の手段・付録 C の §3.4.3）。状態コードの番号は出さない。
function statusMessage(status: number): string {
  if (status === 401) return "ログインが切れました。もう一度ログインしてください。";
  if (status === 403) return "この操作をする権限がありません。必要なら、持ち主か管理者に頼んでください。";
  if (status === 404) return "見つかりません。削除されたか、見る権限がありません。";
  if (status === 409) return "いまの状態では、この操作はできません。画面を開き直してから、もう一度操作してください。";
  if (status === 429) return "利用の上限か、操作の集中のため受け付けられませんでした。少し待ってから、もう一度操作してください。";
  if (status === 503) return "いまは使えません。少し待ってから、もう一度操作してください。";
  if (status >= 500) return "サーバーで問題が起きました。続くときは管理者に連絡してください。";
  return "内容に誤りがあるため、受け付けられませんでした。入力した値を見直してください。";
}

async function ok<T>(res: Response): Promise<T> {
  if (!res.ok) {
    // 検証エラー（400 の {errors:[...]}）は本文をそのまま投げて UI が全件表示できるようにする。文言は 1 件 1 行で、
    // 位置（`description`・`workflows[0]` などの path）を添えない（利用者に出す画面向け・ミニアプリのエディターの
    // [`validationMessages`] と同じ扱い・BUG-80・BUG-121）。位置は `errors` に残るので、欄に写す画面はそれを読む。
    // 構造化した 409（{code, message}・ミニアプリの未公開・版の競合・名前の重複など）は code を添え、message を文言に
    // する。文言の無い誤りは状態コードの既定の文言にする（「API 409」のように番号だけを出さない・GAP-41）。
    let detail = "";
    let errors: GuiValidationError[] | undefined;
    let code: string | undefined;
    try {
      const body = (await res.json()) as { errors?: GuiValidationError[]; code?: unknown; message?: unknown };
      if (body.errors?.length) {
        errors = body.errors;
        detail = body.errors.map((e) => e.message).join("\n");
      }
      if (typeof body.code === "string") code = body.code;
      if (!detail && typeof body.message === "string") detail = body.message;
    } catch {
      // 本文なし
    }
    // 状態コードを添える（見つからない・権限がない＝403/404 を画面が「見つかりません」として出し分ける）。
    throw Object.assign(new Error(detail || statusMessage(res.status)), { status: res.status, errors, code });
  }
  return (await res.json()) as T;
}

/// 応答を読み、失敗なら本文の code・message・検証エラーを添えて投げる（[`errorCode`]・[`validationMessages`] で読む）。
/// アプリを付けた会話の作成（`chat-api` の `createThread`）も同じ扱いにする。
export const okWithReason = ok;

/// 投げられた誤りの code（構造化した誤りのときだけ）。
export function errorCode(e: unknown): string | null {
  const code = (e as { code?: unknown } | null)?.code;
  return typeof code === "string" ? code : null;
}

/// アーティファクトの 409 の code（版の競合・名前の重複・codegen）。
export type ArtifactConflictCode = components["schemas"]["ArtifactConflictCode"];

/// 投げられた誤りが、比べたバージョンの後にほかの人が保存した 409 か（付録 C の §3.5 の出し方にする・GAP-41）。
export function isVersionConflict(e: unknown): boolean {
  const code: ArtifactConflictCode = "version_conflict";
  return (e as { status?: unknown } | null)?.status === 409 && errorCode(e) === code;
}

/// 投げられた誤りが保存時の検証エラーなら、その文言を 1 件 1 行で返す（無ければ null）。
///
/// 位置（`workflows[0]` などの path）は添えない。利用者に出す画面向けで、部品のピンの誤りは文言が部品を別名と版で
/// 言う（BUG-80）。
export function validationMessages(e: unknown): string | null {
  const errors = (e as { errors?: GuiValidationError[] } | null)?.errors;
  return errors?.length ? errors.map((x) => x.message).join("\n") : null;
}

function json(body: unknown): RequestInit {
  return {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  };
}

// ── アーティファクト共通枠（一覧・版・共有）───────────────────────────

type ApiArtifact = {
  id: string;
  kind: ArtifactKind;
  name: string;
  owner: string;
  current_version: number;
  published_version?: number | null;
  created_at: string;
  updated_at: string;
};

function toMeta(a: ApiArtifact): ArtifactMeta {
  return {
    id: a.id,
    kind: a.kind,
    name: a.name,
    owner: a.owner,
    currentVersion: a.current_version,
    publishedVersion: a.published_version ?? null,
    createdAt: a.created_at,
    updatedAt: a.updated_at,
  };
}

/// 一覧の応答（codegen）。`owned_next` は自分のものの、`shared_next_cursor` は共有された分の続きの位置。
type ArtifactListResponse = components["schemas"]["ArtifactListResponse"];

/// 自分が使えるアーティファクト（自分のもの＋共有されたもの・更新の新しい順）の 1 ページを読む関数（[`readFirst`]・
/// [`readMore`]・[`useOwnedSharedList`] に渡す）。続きの位置の扱いは `owned-shared-list.ts` にある。
export function artifactPages(kind: ArtifactKind): FetchPage<ArtifactMeta> {
  return async (req) => {
    const q = new URLSearchParams({ kind });
    if (req.before) {
      q.set("before_updated_at", req.before.updatedAt);
      q.set("before_id", req.before.id);
    }
    if (req.sharedCursor) q.set("shared_cursor", req.sharedCursor);
    const body = await ok<ArtifactListResponse>(await apiFetch(`/artifacts?${q.toString()}`));
    return {
      items: (body.items as ApiArtifact[]).map(toMeta),
      ownedNext: ownedPosition(body.owned_next),
      sharedNext: body.shared_next_cursor ?? null,
    };
  };
}

export async function getArtifact(id: string): Promise<ArtifactMeta> {
  return toMeta(await ok<ApiArtifact>(await apiFetch(`/artifacts/${id}`)));
}

export async function deleteArtifact(id: string): Promise<void> {
  const res = await apiFetch(`/artifacts/${id}`, { method: "DELETE" });
  if (!res.ok) throw new Error(`削除に失敗しました (${res.status})`);
}

export async function listArtifactVersions(id: string): Promise<VersionMeta[]> {
  const data = await ok<{ items: { version: number; created_by: string; created_at: string }[] }>(
    await apiFetch(`/artifacts/${id}/versions`),
  );
  return data.items.map((v) => ({
    version: v.version,
    createdBy: v.created_by,
    createdAt: v.created_at,
  }));
}

export async function shareArtifact(
  id: string,
  target: ShareTarget,
  role: ArtifactRole,
): Promise<void> {
  const res = await apiFetch(`/artifacts/${id}/shares`, { ...json({ target, role }), method: "PUT" });
  if (!res.ok) throw new Error(`共有に失敗しました (${res.status})`);
}

export async function unshareArtifact(
  id: string,
  target: ShareTarget,
  role: ArtifactRole,
): Promise<void> {
  const res = await apiFetch(`/artifacts/${id}/shares`, {
    ...json({ target, role }),
    method: "DELETE",
  });
  if (!res.ok) throw new Error(`共有解除に失敗しました (${res.status})`);
}

export async function listArtifactShares(id: string): Promise<ArtifactShareEntry[]> {
  return ok<ArtifactShareEntry[]>(await apiFetch(`/artifacts/${id}/shares`));
}

// ── skill（Task 6.7）────────────────────────────────────────────────────

export type SkillVersion = { id: string; version: number; body: SkillBody };

export async function createSkill(name: string, body: SkillBody): Promise<SkillVersion> {
  return ok<SkillVersion>(await apiFetch("/skills", json({ name, body })));
}

export async function updateSkill(
  id: string,
  body: SkillBody,
  expectedVersion?: number,
): Promise<SkillVersion> {
  return ok<SkillVersion>(
    await apiFetch(`/skills/${id}`, {
      ...json({ body, expected_version: expectedVersion }),
      method: "PUT",
    }),
  );
}

export async function getSkill(id: string, version?: number): Promise<SkillVersion> {
  const path = version != null ? `/skills/${id}/versions/${version}` : `/skills/${id}`;
  return ok<SkillVersion>(await apiFetch(path));
}

// ── ミニアプリ（Task 6.10）──────────────────────────────────────────────

/// 保存した版（下書き）。`published_version` は公開したバージョン（保存しても変わらない）、`warnings` は画面の
/// 書き損じ（保存はできた。直すまで公開できない・D-16）、`input_warnings` は画面の入力とワークフローの入力項目の
/// 食い違い（保存も公開も止めない・GAP-12）。
export type MiniAppVersion = Omit<components["schemas"]["MiniAppResponse"], "body"> & { body: MiniAppBody };

/// 公開の前の確かめ（公開したバージョンとの差と、公開を止める誤り・codegen）。
export type PublishPreview = components["schemas"]["PublishPreview"];
/// 公開したバージョンから変わるところ 1 つ（`kind` で判別・codegen）。
export type MiniAppChange = components["schemas"]["MiniAppChange"];
/// 公開の結果（codegen）。
export type Publication = components["schemas"]["Publication"];

/// 解決済みのミニアプリ（codegen の応答に、検証済みの本文の型を当てたもの）。
///
/// `unavailable` は使えない部品（壊れた任意のピン・`path` が部品の位置）、`can_edit` は閲覧者本人が編集できるか、
/// `can_share` は共有できるか（owner だけ・BUG-78）。
export type ResolvedMiniApp = Omit<components["schemas"]["ResolvedMiniAppResponse"], "body"> & {
  /// 検証済みの本文（画面＝Yui の ```ui を含む Markdown と、束ねたワークフロー）。
  body: MiniAppBody;
};

/// 使えない部品の理由 1 件（codegen）。
export type UnavailablePin = components["schemas"]["GuiValidationError"];

/// 画面のボタンで起動したワークフロー 1 件（本人の分・#604・codegen）。
export type MiniAppRun = components["schemas"]["MiniAppRun"];

export async function createMiniApp(name: string, body: MiniAppBody): Promise<MiniAppVersion> {
  return ok<MiniAppVersion>(await apiFetch("/mini-apps", json({ name, body })));
}

export async function updateMiniApp(
  id: string,
  body: MiniAppBody,
  expectedVersion?: number,
): Promise<MiniAppVersion> {
  return ok<MiniAppVersion>(
    await apiFetch(`/mini-apps/${id}`, {
      ...json({ body, expected_version: expectedVersion }),
      method: "PUT",
    }),
  );
}

/// 束ねた部品 1 つの、呼んだ人が読める最新のバージョンと、上げると変わるところ（codegen・GAP-11）。
export type PinUpdate = components["schemas"]["PinUpdate"];
/// 部品を最新のバージョンに上げると変わるところ 1 つ（`kind` で判別・codegen）。
export type PinDiff = components["schemas"]["PinDiff"];

/// 束ねるワークフローと保存ビューごとに、新しいバージョンと上げると変わるところを読む（あなたの権限で読む・リクエストの
/// 順・GAP-11）。差の種類はサーバーが決める。読めない部品は `readable: false`。
export async function getMiniAppPinUpdates(pins: {
  workflows: { artifact_id: string; version: number }[];
  data_views: { artifact_id: string; version: number }[];
}): Promise<components["schemas"]["PinUpdatesResponse"]> {
  return ok<components["schemas"]["PinUpdatesResponse"]>(await apiFetch("/mini-apps/pin-updates", json(pins)));
}

/// 束ねるワークフローの版の入力の手がかり（読めたか・入力項目の並びの順・codegen）。
export type PinnedWorkflowInputs = components["schemas"]["PinnedWorkflowInputs"];

/// 束ねるワークフローの版ごとの入力の手がかりを読む（あなたの権限で読む・リクエストの順・GAP-12）。手がかりの決まりは
/// サーバーにだけ置く（保存のときの突き合わせと同じもの）。読めない版は `readable: false`。
export async function getMiniAppWorkflowInputs(
  workflows: { artifact_id: string; version: number }[],
): Promise<PinnedWorkflowInputs[]> {
  const res = await ok<components["schemas"]["WorkflowInputsResponse"]>(
    await apiFetch("/mini-apps/workflow-inputs", json({ workflows })),
  );
  return res.items;
}

/// ミニアプリを開く。`version` を省くと公開したバージョン（まだ公開していないアプリを作る人が開くときは最新）。
/// 使う人がまだ公開していないアプリを開くと、code が `not_published` の誤りになる（D-16）。
export async function resolveMiniApp(id: string, version?: number): Promise<ResolvedMiniApp> {
  const qs = version != null ? `?version=${version}` : "";
  return ok<ResolvedMiniApp>(await apiFetch(`/mini-apps/${id}/resolved${qs}`));
}

/// 公開の前の確かめ（作る人だけ）: そのバージョンと公開しているバージョンの差と、公開を止める誤り。
export async function previewMiniAppPublish(id: string, version: number): Promise<PublishPreview> {
  return ok<PublishPreview>(await apiFetch(`/mini-apps/${id}/publish-preview?version=${version}`));
}

/// バージョンを公開する（作る人だけ）。前に公開していたバージョンを選ぶと公開を戻せる。
export async function publishMiniApp(id: string, version: number): Promise<Publication> {
  return ok<Publication>(await apiFetch(`/mini-apps/${id}/publish`, json({ version })));
}

// ── 使う人の権限の確かめ（GAP-37・codegen）──────────────────────────────

/// 作る人向けの確かめの結果（部品が要る権限と、共有の相手ごとの結果）。
export type AccessReport = components["schemas"]["AccessReport"];
/// 部品が要る権限 1 つ。
export type AccessNeedView = components["schemas"]["AccessNeedView"];
/// 足りるか（ok・limited・unchecked・missing）。
export type AccessStatus = components["schemas"]["AccessStatus"];
/// 権限を確かめた部品（ワークフロー・保存ビュー・スキル）。
export type AccessPart = components["schemas"]["AccessPart"];
/// 相手 1 件の結果。
export type SubjectAccess = components["schemas"]["SubjectAccess"];
/// 使う人向けの確かめの結果（足りない・範囲が狭い・使えない部品だけ）。
export type MyAppAccess = components["schemas"]["MyAppAccess"];
export type MyPartAccess = components["schemas"]["MyPartAccess"];
export type MyAccessItem = components["schemas"]["MyAccessItem"];

/// アプリを使う人の権限を確かめる（作る人だけ）。`version` を省くと公開しているバージョン（まだ公開していなければ
/// 最新）、`subjects` を省くと共有している相手（相手ごとの結果は持ち主にだけ返る）。
export async function checkMiniAppAccess(
  id: string,
  opts: { version?: number; subjects?: ShareTarget[] } = {},
): Promise<AccessReport> {
  return ok<AccessReport>(await apiFetch(`/mini-apps/${id}/access-check`, json(opts)));
}

/// ワークフローを使う人の権限を確かめる（作る人だけ・最新のバージョン）。
export async function checkWorkflowAccess(
  id: string,
  opts: { subjects?: ShareTarget[] } = {},
): Promise<AccessReport> {
  return ok<AccessReport>(await apiFetch(`/workflows/${id}/access-check`, json(opts)));
}

/// アプリを使う人自身に足りない権限（開いているバージョン）。
export async function getMyMiniAppAccess(id: string, version: number): Promise<MyAppAccess> {
  return ok<MyAppAccess>(await apiFetch(`/mini-apps/${id}/access?version=${version}`));
}

// ── 図のボタンの実行（Task 6.5・#604・action_id + params のみ送れる）─────

export type UiActionResult = { result: Record<string, unknown> };

/// 同じ図の同じ操作が既に実行済み（サーバが二重送信を拒否した・#410）。
///
/// 表示の遅れ（別タブ・戻る操作・再送）で二度押しても事故にならないよう、UI はこれを
/// 失敗ではなく「送信済み」として扱い、図を送信済み表示へ倒す。
export class UiActionAlreadyInvoked extends Error {
  constructor() {
    super("この操作は送信済みです");
    this.name = "UiActionAlreadyInvoked";
  }
}

/// 本文の図（```ui）のボタンの実行（#604）。`actionId` は `{ブロックの識別子}:{a1}`。
///
/// ワークフローのボタンは送信ごとの `idempotencyKey` が要る（同じキーの再送は前の起動の結果を返す）。
/// 誤りは状況ごとに分けて投げる（画面が文言を出し分ける）。
export async function invokeChatYuiAction(
  threadId: string,
  messageId: string,
  actionId: string,
  values: Record<string, unknown>,
  idempotencyKey?: string,
): Promise<UiActionResult> {
  let res: Response;
  try {
    res = await apiFetch(
      `/threads/${threadId}/messages/${messageId}/ui-actions`,
      json({
        action_id: actionId,
        params: values,
        ...(idempotencyKey ? { idempotency_key: idempotencyKey } : {}),
      }),
    );
  } catch {
    throw new UiActionUncertain();
  }
  throwIfServerError(res.status);
  if (res.status === 409) throw new UiActionAlreadyInvoked();
  if (res.status === 403) throw new UiActionForbidden();
  if (res.status === 404) throw new UiActionNotFound();
  return ok<UiActionResult>(res);
}

/// ボタンの送信のサーバーの誤り（5xx）を、起きたことごとに分けて投げる（GAP-41）。
///
/// - 502・504（間の中継が応答を待てなかった）: サーバーは受け付けたかもしれない（[`UiActionUncertain`]）。
/// - 503（この環境でボタンの実行先が使えない）: 受け付けていない（[`UiActionUnavailable`]）。
/// - そのほか（500 など）: サーバーで問題が起きた（[`UiActionServerError`]）。
///
/// どれも押し直しは同じ冪等キーで送る（受け付けていた場合に二つ目の run を起動しない）。
function throwIfServerError(status: number): void {
  if (status === 502 || status === 504) throw new UiActionUncertain();
  if (status === 503) throw new UiActionUnavailable();
  if (status >= 500) throw new UiActionServerError();
}

/// 送信の結果が分からない（通信が途中で切れた・サーバーの障害）。受け付けられたかもしれないので、
/// 押し直すときは同じ冪等キーで送る。
export class UiActionUncertain extends Error {
  constructor() {
    super("送信の結果を確かめられませんでした。もう一度押すと、同じ送信として扱います");
    this.name = "UiActionUncertain";
  }
}

/// ボタンの実行先が、いまこの環境で使えない（503・受け付けていない）。
export class UiActionUnavailable extends Error {
  constructor() {
    super("このボタンはいま使えません。少し待ってから、もう一度押してください。");
    this.name = "UiActionUnavailable";
  }
}

/// サーバーで問題が起きた（500 など）。押し直すときは同じ冪等キーで送る（受け付けていても二重には起動しない）。
export class UiActionServerError extends Error {
  constructor() {
    super("サーバーで問題が起きました。もう一度押しても続くときは、管理者に連絡してください。");
    this.name = "UiActionServerError";
  }
}

/// 会話の閲覧者が押した（ボタンは編集者だけが使える）。
export class UiActionForbidden extends Error {
  constructor(message = "この会話は閲覧のみのため、ボタンを使えません") {
    super(message);
    this.name = "UiActionForbidden";
  }
}

/// ボタンが見つからない（本文に無い・ワークフローを解決できなかった・権限が無い）。
export class UiActionNotFound extends Error {
  constructor() {
    super("このボタンの実行先が見つからないか、使う権限がありません");
    this.name = "UiActionNotFound";
  }
}

/// 開いた後に、アプリの公開しているバージョンが変わった（またはまだ公開していない）。画面を開き直すと押せる（D-16）。
/// 開いた後に公開したバージョンが変わった（`app_version_changed`・`not_published`）。文言はサーバーの `message`
/// （新しいバージョンへ変えたのか戻したのかを決めつけない・`gui_errors.rs`）。
export class UiActionAppChanged extends Error {
  constructor(message: string) {
    super(message);
    this.name = "UiActionAppChanged";
  }
}

/// ミニアプリの画面の ```ui のボタンを実行する（#604）。誤りの投げ分けは会話のボタンと同じ。
/// 409 のうち、開いた後に公開したバージョンが変わったもの（`app_version_changed`・`not_published`）は二重送信と
/// 分けて投げる。
export async function invokeMiniAppYuiAction(
  appId: string,
  version: number,
  actionId: string,
  values: Record<string, unknown>,
  idempotencyKey: string,
): Promise<UiActionResult> {
  let res: Response;
  try {
    res = await apiFetch(
      `/mini-apps/${appId}/ui-actions`,
      json({ version, action_id: actionId, params: values, idempotency_key: idempotencyKey }),
    );
  } catch {
    throw new UiActionUncertain();
  }
  throwIfServerError(res.status);
  if (res.status === 409) {
    const body = (await res.json().catch(() => null)) as { code?: unknown; message?: unknown } | null;
    if (body?.code === "app_version_changed" || body?.code === "not_published") {
      throw new UiActionAppChanged(typeof body.message === "string" ? body.message : "画面を開き直してください。");
    }
    throw new UiActionAlreadyInvoked();
  }
  if (res.status === 403) throw new UiActionForbidden("このアプリを使う権限がありません");
  if (res.status === 404) throw new UiActionNotFound();
  return ok<UiActionResult>(res);
}

/// 本人がミニアプリの画面のボタンで起動した記録（新しい 20 件を古い順に）。
/// ワークフローが画面に返した結果（`reply`）は、未確定の run と最新の終わった 1 件にだけ付く（GAP-09）。
export async function listMiniAppRuns(appId: string): Promise<MiniAppRun[]> {
  const res = await ok<{ runs: MiniAppRun[] }>(await apiFetch(`/mini-apps/${appId}/runs`));
  return res.runs;
}

/// 本人が起動した 1 件を、画面に返した結果つきで読む（一覧で結果が付かない古い行を開いたとき）。
export async function getMiniAppRun(appId: string, runId: string): Promise<MiniAppRun> {
  return ok<MiniAppRun>(await apiFetch(`/mini-apps/${appId}/runs/${runId}`));
}

/// 回答の本文の ```ui app を、そのボタンの実行先ごとミニアプリとして保存する（#604）。
export async function saveYuiBlockAsApp(
  threadId: string,
  messageId: string,
  block: string,
  name: string,
  description?: string,
): Promise<MiniAppVersion> {
  return ok<MiniAppVersion>(
    await apiFetch(
      `/threads/${threadId}/messages/${messageId}/save-as-app`,
      json({ block, name, ...(description ? { description } : {}) }),
    ),
  );
}
