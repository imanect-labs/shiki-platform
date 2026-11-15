"use client";
import * as React from "react";
import { Check, FileUp, Network, RefreshCw, WandSparkles } from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogTitle,
  DialogDescription,
} from "@/components/ui/dialog";
import { AdminPage, Panel } from "../admin-page";
import {
  request,
  post,
  hasChanges,
  newChanges,
  type Info,
  type Changes,
  type Group,
  type RoleEdit,
  type Plan,
  type Operation,
  type RetryInput,
  type Role,
} from "./api";
import { Source } from "./source";
import { Roles } from "./roles";
import { AutoDialog } from "./auto";
import { ConnectionDialog } from "./connection";
import { ImportDialog } from "./import";
import { MappingDiff, RoleTree, ReviewSources } from "./review";

export function DirectoryView({ rolesOnly = false }: { rolesOnly?: boolean }) {
  const [info, setInfo] = React.useState<Info>();
  const [revision, setRevision] = React.useState(0);
  const [selected, setSelected] = React.useState<Group>();
  const [sources, setSources] = React.useState<Record<string, Group>>({});
  const [changes, setChanges] = React.useState<Changes>(() => ({
    idempotency_key: "",
    roles: [],
    mappings: [],
  }));
  const [beforeRoles, setBeforeRoles] = React.useState<Role[]>([]);
  const [nodes, setNodes] = React.useState<RoleEdit[]>([]);
  const [dialog, setDialog] = React.useState<
    "auto" | "import" | "connection" | "review"
  >();
  const [operation, setOperation] = React.useState<Operation>();
  const [history, setHistory] = React.useState<Operation[]>([]);
  const [busy, setBusy] = React.useState(false);
  const [recovering, setRecovering] = React.useState(false);
  const [recoveryConfirmed, setRecoveryConfirmed] = React.useState(false);
  const [error, setError] = React.useState("");
  const [message, setMessage] = React.useState("");
  const [reviewGroup, setReviewGroup] = React.useState<Group>();
  const retryOperation = async (confirmed: boolean) => {
    if (!operation) return;
    setBusy(true);
    setError("");
    try {
      setOperation(
        await post<Operation>(`/operations/${operation.id}/retry`, {
          recovery_confirmed: confirmed,
        } satisfies RetryInput),
      );
      setRecovering(false);
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  };
  const dirty = hasChanges(changes);
  const pending = !!operation && operation.state !== "succeeded";
  const disabled = busy || pending || !info?.keycloak_available;
  React.useEffect(() => {
    const controller = new AbortController();
    Promise.all([
      request<Info>("", { signal: controller.signal }),
      request<Operation[]>("/operations", { signal: controller.signal }),
    ])
      .then(([info, operations]) => {
        setInfo(info);
        setHistory(operations);
        setOperation(operations.find((op) => op.state !== "succeeded"));
      })
      .catch((e) => {
        if (!controller.signal.aborted) setError(e.message);
      });
    return () => controller.abort();
  }, [revision]);
  React.useEffect(() => {
    if (
      !operation ||
      operation.state === "succeeded" ||
      operation.state === "failed"
    )
      return;
    const controller = new AbortController();
    const timer = setInterval(() => {
      request<Operation>(`/operations/${operation.id}`, {
        signal: controller.signal,
      })
        .then((next) => {
          setOperation(next);
          setError("");
          if (next.state === "succeeded") {
            setChanges(newChanges());
            setNodes([]);
            setSelected(undefined);
            setSources({});
            setRevision((old) => old + 1);
            setMessage(
              "反映が完了しました。ユーザーの所属は次回ログイン・セッション更新時に同期されます。",
            );
          }
        })
        .catch((e) => {
          if (!controller.signal.aborted)
            setError(
              `進捗を取得できません。処理はサーバで継続します。${e.message}`,
            );
        });
    }, 1500);
    return () => {
      clearInterval(timer);
      controller.abort();
    };
  }, [operation]);
  const mutate = (next: Changes) => {
    setChanges({ ...next, idempotency_key: crypto.randomUUID() });
    setError("");
    setMessage("");
  };
  const choose = (group: Group) => {
    setSelected(group);
    setSources((old) => ({ ...old, [group.id]: group }));
  };
  const mapping =
    selected &&
    (changes.mappings.find((m) => m.group_id === selected.id)?.role_ids ??
      selected.role_ids);
  const changeMapping = (ids: string[]) => {
    if (!selected) return;
    mutate({
      ...changes,
      mappings: [
        ...changes.mappings.filter((m) => m.group_id !== selected.id),
        {
          group_id: selected.id,
          role_ids: ids,
          expected_version: selected.mapping_version,
        },
      ],
    });
  };
  const saveRole = (role: RoleEdit, assign: boolean) => {
    const previous = changes.roles.find((r) => r.id === role.id);
    const next = {
      ...role,
      expected_version: previous?.expected_version ?? role.expected_version,
    };
    const mappings =
      assign && selected
        ? [
            ...changes.mappings.filter((m) => m.group_id !== selected.id),
            {
              group_id: selected.id,
              expected_version: selected.mapping_version,
              role_ids: [...new Set([...(mapping ?? []), role.id])],
            },
          ]
        : changes.mappings;
    mutate({
      ...changes,
      roles: [...changes.roles.filter((r) => r.id !== role.id), next],
      mappings,
    });
    setNodes((old) => [...old.filter((r) => r.id !== role.id), next]);
  };
  const accept = (plan: Plan) => {
    const groups = {
      ...sources,
      ...Object.fromEntries(plan.sources.map((g) => [g.id, g])),
    };
    setSources(groups);
    // 自動作成・CSVは保存済み状態に対する案。手編集があれば上書き前に利用者が取り消す。
    mutate(plan.changes);
    setNodes(plan.nodes);
    setBeforeRoles(plan.before_roles);
    setReviewGroup(plan.sources[0]);
    setDialog("review");
  };
  const preview = async () => {
    setBusy(true);
    setError("");
    try {
      const plan = await post<Plan>("/preview", changes);
      setNodes(plan.nodes);
      setBeforeRoles(plan.before_roles);
      setSources((old) => ({
        ...old,
        ...Object.fromEntries(plan.sources.map((g) => [g.id, g])),
      }));
      setReviewGroup(plan.sources[0]);
      setDialog("review");
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  };
  const submit = async () => {
    setBusy(true);
    setError("");
    try {
      const op = await post<Operation>("/apply", changes);
      setOperation(op);
      setDialog(undefined);
      if (op.state === "succeeded") {
        setChanges(newChanges());
        setNodes([]);
        setSelected(undefined);
        setRevision((old) => old + 1);
      }
    } catch (e) {
      setError((e as Error).message);
      setRevision((old) => old + 1);
    } finally {
      setBusy(false);
    }
  };
  const sync = async () => {
    setBusy(true);
    setError("");
    try {
      const op = await post<Operation>("/sync", {
        idempotency_key: crypto.randomUUID(),
      });
      setOperation(op);
    } catch (e) {
      setError((e as Error).message);
      setRevision((old) => old + 1);
    } finally {
      setBusy(false);
    }
  };
  const reviewSources = changes.mappings
    .map((m) => sources[m.group_id])
    .filter((g): g is Group => !!g);
  return (
    <AdminPage
      title={rolesOnly ? "ロール" : "AD連携"}
      description={
        rolesOnly
          ? "組織の階層付きロールを管理します。子のメンバーは親にも含まれます。"
          : "ADグループとShikiのロールの対応を管理します。"
      }
      wide
      actions={
        <>
          {!rolesOnly && (
            <>
              <Button
                variant="outline"
                size="sm"
                disabled={busy || (pending && operation?.state !== "failed")}
                onClick={() => setDialog("connection")}
              >
                <Network aria-hidden />
                接続設定
              </Button>
              <Button
                variant="outline"
                size="sm"
                disabled={disabled || dirty}
                onClick={sync}
              >
                <RefreshCw aria-hidden />
                ADを同期
              </Button>
              <Button
                variant="outline"
                size="sm"
                disabled={disabled || dirty}
                onClick={() => setDialog("auto")}
              >
                <WandSparkles aria-hidden />
                ADから自動作成
              </Button>
              <Button
                variant="outline"
                size="sm"
                disabled={disabled || dirty}
                onClick={() => setDialog("import")}
              >
                <FileUp aria-hidden />
                CSVインポート
              </Button>
            </>
          )}
          <Button
            size="sm"
            variant="ghost"
            disabled={busy || dirty}
            onClick={() => setRevision((old) => old + 1)}
          >
            <RefreshCw aria-hidden />
            再読込
          </Button>
        </>
      }
    >
      {!info ? (
        <p role="status" className="text-xs text-muted-foreground">
          管理情報を取得中…
        </p>
      ) : (
        !info.keycloak_available && (
          <p role="alert" className="rounded-md border p-3 text-xs leading-6">
            Keycloakの管理接続が未設定です。運用者がプロビジョナ接続を設定すると編集できます。
          </p>
        )
      )}
      {error && (
        <p
          role="alert"
          className="rounded-md border border-destructive/30 p-3 text-xs leading-6 text-destructive"
        >
          {error}
        </p>
      )}
      {operation && (
        <Panel>
          <div className="flex flex-wrap items-center justify-between gap-3 p-4">
            <div className="min-w-0">
              <p className="text-sm font-medium">
                {operation.state === "succeeded"
                  ? "反映完了"
                  : operation.state === "failed"
                    ? "反映が停止しました"
                    : "サーバで反映中"}
              </p>
              <p className="mt-1 text-xs leading-5 text-muted-foreground">
                {operation.processed} / {operation.total}件
                {operation.state === "failed"
                  ? ` · ${operation.error_code === "execution_interrupted" ? "実行が中断されています。運用者に以前の実行と外部反映の状態を確認してもらってください" : operation.error_code === "authorization_revoked" ? "操作した管理者の権限を確認してください" : operation.error_code === "version_conflict" ? "保存内容との競合が発生しました" : "Keycloak・AD・認可サービスへの接続を確認して再実行してください"}`
                  : operation.state !== "succeeded"
                    ? " · 画面を閉じても処理は継続します"
                    : ""}
              </p>
            </div>
            {operation.state === "failed" && (
              <Button
                size="sm"
                disabled={busy}
                onClick={() => {
                  if (operation.error_code === "execution_interrupted") {
                    setRecoveryConfirmed(false);
                    setError("");
                    setRecovering(true);
                  } else void retryOperation(false);
                }}
              >
                続きから再実行
              </Button>
            )}
          </div>
          {operation.state !== "failed" && (
            <progress
              className="block h-1 w-full accent-primary"
              value={operation.processed}
              max={Math.max(1, operation.total)}
              aria-label="反映の進捗"
            />
          )}
        </Panel>
      )}
      <Panel>
        <div
          className={
            rolesOnly
              ? "flex h-[560px] flex-col"
              : "grid lg:grid-cols-[minmax(280px,0.85fr)_minmax(0,1.6fr)]"
          }
        >
          {!rolesOnly && (
            <section className="flex h-[400px] min-w-0 flex-col border-b lg:h-[560px] lg:border-b-0 lg:border-r">
              <Source
                key={revision}
                revision={revision}
                selected={selected?.id}
                onSelect={choose}
              />
            </section>
          )}
          <section className="flex h-[560px] min-w-0 flex-col">
            <div className="border-b px-4 py-3">
              <h2 className="truncate text-sm font-semibold">
                {rolesOnly
                  ? "ロールの管理"
                  : (selected?.name ?? "ADグループを選択")}
              </h2>
              <p className="mt-1 truncate text-[11px] text-muted-foreground">
                {rolesOnly
                  ? "名前・親の変更は確認してから保存します。"
                  : selected
                    ? selected.ou_path.join(" / ") || "OUなし"
                    : "左のOUを開くか、グループを検索してください。"}
              </p>
            </div>
            {rolesOnly || selected ? (
              <Roles
                key={revision}
                revision={revision}
                prefix={info?.role_id_prefix ?? ""}
                drafts={changes.roles}
                selected={rolesOnly ? undefined : mapping}
                onChange={rolesOnly ? undefined : changeMapping}
                onSave={saveRole}
                disabled={disabled}
              />
            ) : (
              <div className="flex flex-1 items-center justify-center px-6 text-xs text-muted-foreground">
                グループを選ぶと、対応するロールを編集できます。
              </div>
            )}
          </section>
        </div>
      </Panel>
      {selected && (
        <details className="rounded-md border px-4 py-3 text-xs">
          <summary className="cursor-pointer text-muted-foreground">
            グループの識別情報
          </summary>
          <dl className="mt-3 space-y-2 break-all text-[11px]">
            <dt className="font-medium">group_id（CSV用）</dt>
            <dd className="select-all font-mono">{selected.id}</dd>
            <dt className="font-medium">ADのDN</dt>
            <dd>{selected.distinguished_name ?? "未取得"}</dd>
            <dt className="font-medium">認証グループのパス</dt>
            <dd>{selected.claim_path}</dd>
          </dl>
        </details>
      )}
      {dirty && (
        <div className="sticky bottom-2 z-10 flex flex-wrap items-center justify-between gap-3 rounded-lg border bg-card px-4 py-3 shadow-sm">
          <div>
            <p className="text-[13px] font-medium">
              {changes.mappings.length}グループ・{changes.roles.length}
              ロールに未反映の変更
            </p>
            <p className="mt-1 text-[11px] text-muted-foreground">
              保存するか取り消すと、CSV・自動作成・再同期を利用できます。
            </p>
          </div>
          <div className="flex gap-2">
            <Button
              size="sm"
              variant="outline"
              disabled={disabled}
              onClick={() => {
                setChanges(newChanges());
                setNodes([]);
                setMessage("未反映の変更を取り消しました。");
              }}
            >
              変更を取り消す
            </Button>
            <Button size="sm" disabled={disabled} onClick={preview}>
              <Check aria-hidden />
              変更を確認
            </Button>
          </div>
        </div>
      )}
      <p role="status" className="text-xs leading-6 text-muted-foreground">
        {message ||
          "対応するロールは、ユーザーの検証済みADグループから解決されます。反映には次回ログインまたはセッション更新が必要です。"}
      </p>
      {!!history.length && (
        <details className="rounded-md border px-4 py-3 text-xs">
          <summary className="cursor-pointer text-muted-foreground">
            操作履歴（直近20件）
          </summary>
          <ul className="mt-3 divide-y">
            {history.map((op) => (
              <li key={op.id} className="flex flex-wrap gap-2 py-2">
                <span className="min-w-0 flex-1 break-all font-mono text-[10px]">
                  {op.id}
                </span>
                <span>
                  {op.state === "succeeded"
                    ? "完了"
                    : op.state === "failed"
                      ? "停止"
                      : "処理中"}{" "}
                  · {op.processed}/{op.total}件
                </span>
              </li>
            ))}
          </ul>
        </details>
      )}
      {recovering && (
        <Dialog
          open
          onOpenChange={(open) => {
            if (!open && !busy) setRecovering(false);
          }}
        >
          <DialogContent>
            <DialogTitle>中断した反映の再実行</DialogTitle>
            <DialogDescription>
              外部サービスで受付済みの変更が残っている可能性があります。運用者が以前の実行を停止し、反映状態を確認してから続きに進んでください。
            </DialogDescription>
            <p className="break-all rounded-md bg-muted px-3 py-2 font-mono text-[11px] text-muted-foreground">
              操作ID: {operation?.id}
            </p>
            <label className="flex items-start gap-2 text-xs leading-6">
              <input
                type="checkbox"
                checked={recoveryConfirmed}
                onChange={(e) => setRecoveryConfirmed(e.target.checked)}
                disabled={busy}
                className="mt-1.5 size-3.5 shrink-0 accent-primary"
              />
              運用者が以前の実行の停止と、Keycloak・認可サービスの反映状態を確認済みです
            </label>
            {error && (
              <p role="alert" className="text-xs text-destructive">
                {error}
              </p>
            )}
            <div className="flex justify-end gap-2">
              <Button
                variant="outline"
                disabled={busy}
                onClick={() => setRecovering(false)}
              >
                戻る
              </Button>
              <Button
                disabled={!recoveryConfirmed || busy}
                onClick={() => void retryOperation(true)}
              >
                確認して再実行
              </Button>
            </div>
          </DialogContent>
        </Dialog>
      )}
      {dialog === "connection" && (
        <ConnectionDialog
          allowed={info?.ldap_allowed_hosts ?? []}
          onClose={() => setDialog(undefined)}
          onSaved={() =>
            setMessage("接続設定を保存しました。ADを同期してください。")
          }
        />
      )}
      {dialog === "auto" && (
        <AutoDialog
          selected={selected}
          revision={revision}
          onClose={() => setDialog(undefined)}
          onAccept={accept}
        />
      )}
      {dialog === "import" && (
        <ImportDialog onClose={() => setDialog(undefined)} onAccept={accept} />
      )}
      {dialog === "review" && (
        <Dialog
          open
          onOpenChange={(open) => {
            if (!open && !busy) setDialog(undefined);
          }}
        >
          <DialogContent className="flex h-[min(780px,90dvh)] max-w-6xl flex-col gap-0 overflow-hidden p-0 max-sm:max-h-[94dvh] max-sm:px-0 max-sm:pb-0 max-sm:pt-0">
            <header className="shrink-0 border-b px-5 py-4 pr-12">
              <DialogTitle className="text-base">変更内容の確認</DialogTitle>
              <DialogDescription className="mt-1 text-xs">
                {changes.roles.length}ロール・{changes.mappings.length}
                グループに反映します。既存のIDは維持されます。
              </DialogDescription>
            </header>
            <div className="grid min-h-0 flex-1 grid-cols-1 overflow-y-auto md:grid-cols-[300px_minmax(0,1fr)] md:overflow-hidden">
              <aside className="flex min-h-32 flex-col border-b md:min-h-0 md:border-b-0 md:border-r">
                <ReviewSources
                  sources={reviewSources}
                  active={reviewGroup?.id}
                  onSelect={setReviewGroup}
                />
              </aside>
              <section className="flex min-h-64 min-w-0 flex-col md:min-h-0">
                <RoleTree
                  nodes={nodes.length ? nodes : changes.roles}
                  changes={changes}
                />
                <RoleChanges
                  changes={changes}
                  before={beforeRoles}
                  nodes={nodes}
                />
                <MappingDiff
                  group={reviewGroup}
                  changes={changes}
                  nodes={nodes}
                />
              </section>
            </div>
            <footer className="flex shrink-0 flex-wrap justify-between gap-3 border-t px-5 py-3">
              <p className="text-xs text-muted-foreground">
                ロールの親子関係・対応付けは権限に影響します。
              </p>
              <div className="flex gap-2">
                <Button
                  variant="outline"
                  disabled={busy}
                  onClick={() => setDialog(undefined)}
                >
                  編集に戻る
                </Button>
                <Button disabled={busy || !dirty} onClick={submit}>
                  {busy ? "受け付け中…" : "反映する"}
                </Button>
              </div>
              {error && (
                <p role="alert" className="w-full text-xs text-destructive">
                  {error}
                </p>
              )}
            </footer>
          </DialogContent>
        </Dialog>
      )}
    </AdminPage>
  );
}

function RoleChanges({
  changes,
  before,
  nodes,
}: {
  changes: Changes;
  before: Role[];
  nodes: RoleEdit[];
}) {
  if (!before.length) return null;
  const index = new Map(nodes.map((r) => [r.id, r]));
  const path = (id: string) => {
    const parts = [];
    const seen = new Set();
    let role = index.get(id);
    while (role && !seen.has(role.id)) {
      seen.add(role.id);
      parts.unshift(role.label);
      role = role.parent_id ? index.get(role.parent_id) : undefined;
    }
    return parts.join(" / ");
  };
  return (
    <div className="max-h-36 shrink-0 overflow-auto border-t px-4 py-3 text-xs">
      <p className="mb-2 font-semibold">既存ロールの変更</p>
      {before
        .filter((role) => changes.roles.some((r) => r.id === role.id))
        .map((role) => (
          <div key={role.id} className="py-1">
            <p className="text-muted-foreground">
              {role.path_labels.join(" / ")}
            </p>
            <p className="mt-1">→ {path(role.id)}</p>
            {role.child_count > 0 &&
              changes.roles.find((r) => r.id === role.id)?.parent_id !==
                role.parent_id && (
                <p className="mt-1 text-muted-foreground">
                  直属の子ロール{role.child_count}
                  件と、その配下のメンバーの継承先も変わります。
                </p>
              )}
          </div>
        ))}
    </div>
  );
}
