"use client";
import * as React from "react";
import { ChevronRight, Folder, Pencil, Plus, Search } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { IconInput } from "@/components/ui/icon-input";
import {
  Dialog,
  DialogContent,
  DialogTitle,
  DialogDescription,
} from "@/components/ui/dialog";
import {
  request,
  params,
  rolePath,
  type Role,
  type RolePage,
  type RoleEdit,
} from "./api";

export function Roles({
  revision,
  selected,
  onChange,
  drafts,
  onSave,
  prefix,
  disabled = false,
  picker = false,
  exclude,
}: {
  revision: number;
  selected?: string[];
  onChange?: (ids: string[]) => void;
  drafts?: RoleEdit[];
  onSave?: (role: RoleEdit, assign: boolean) => void;
  prefix: string;
  disabled?: boolean;
  picker?: boolean;
  exclude?: string;
}) {
  const [draftPage, setDraftPage] = React.useState(0);
  const [query, setQuery] = React.useState("");
  const [parent, setParent] = React.useState<Role | null>(null);
  const [cursor, setCursor] = React.useState<string | null>(null);
  const [page, setPage] = React.useState<RolePage>();
  const [known, setKnown] = React.useState<Record<string, Role>>({});
  const [error, setError] = React.useState("");
  const [loading, setLoading] = React.useState(true);
  const [editor, setEditor] = React.useState<{ role?: Role }>();
  React.useEffect(() => {
    const controller = new AbortController();
    setLoading(true);
    setError("");
    const timer = setTimeout(
      () => {
        request<RolePage>(
          `/roles${params({ q: query, parent: parent?.id, cursor })}`,
          { signal: controller.signal },
        )
          .then((page) => {
            setPage(page);
            setKnown((previous) => ({
              ...previous,
              ...Object.fromEntries(page.items.map((role) => [role.id, role])),
            }));
          })
          .catch((e) => {
            if (!controller.signal.aborted) setError(e.message);
          })
          .finally(() => {
            if (!controller.signal.aborted) setLoading(false);
          });
      },
      query ? 200 : 0,
    );
    return () => {
      clearTimeout(timer);
      controller.abort();
    };
  }, [query, parent?.id, cursor, revision]);
  const missing =
    selected
      ?.filter((id) => !known[id] && !drafts?.some((r) => r.id === id))
      .sort()
      .join(",") ?? "";
  React.useEffect(() => {
    if (!missing) return;
    const controller = new AbortController();
    (async () => {
      const ids = missing.split(",");
      for (let i = 0; i < ids.length; i += 100) {
        const page = await request<RolePage>(
          `/roles${params({ ids: ids.slice(i, i + 100).join(",") })}`,
          { signal: controller.signal },
        );
        setKnown((old) => ({
          ...old,
          ...Object.fromEntries(page.items.map((r) => [r.id, r])),
        }));
      }
    })().catch((e) => {
      if (!controller.signal.aborted) setError(e.message);
    });
    return () => controller.abort();
  }, [missing]);
  const change = (id: string, checked: boolean) =>
    onChange?.(
      checked
        ? [...new Set([...(selected ?? []), id])]
        : (selected ?? []).filter((value) => value !== id),
    );
  const navigate = (role: Role | null) => {
    setParent(role);
    setCursor(null);
    setDraftPage(0);
    setQuery("");
  };
  const draftIndex = new Map(drafts?.map((r) => [r.id, r]));
  const draftChildren = new Map<string, number>();
  for (const row of drafts ?? []) {
    if (row.parent_id)
      draftChildren.set(
        row.parent_id,
        (draftChildren.get(row.parent_id) ?? 0) + 1,
      );
  }
  const draftView = (row: RoleEdit): Role => {
    const ids: string[] = [];
    const labels: string[] = [];
    const seen = new Set<string>();
    let current: RoleEdit | undefined = row;
    while (current && !seen.has(current.id) && ids.length <= 128) {
      seen.add(current.id);
      ids.unshift(current.id);
      labels.unshift(current.label);
      const next: RoleEdit | undefined = current.parent_id
        ? draftIndex.get(current.parent_id)
        : undefined;
      if (!next && current.parent_id && known[current.parent_id]) {
        ids.unshift(...known[current.parent_id].path_ids);
        labels.unshift(...known[current.parent_id].path_labels);
      }
      current = next;
    }
    return {
      id: row.id,
      label: row.label,
      parent_id: row.parent_id,
      version: row.expected_version,
      path_ids: ids,
      path_labels: labels,
      child_count: draftChildren.get(row.id) ?? 0,
    };
  };
  const pendingRows =
    drafts?.filter(
      (r) =>
        !known[r.id] &&
        r.id !== exclude &&
        (!exclude || !draftView(r).path_ids.includes(exclude)) &&
        (query
          ? r.label.toLowerCase().includes(query.toLowerCase())
          : (r.parent_id ?? null) === (parent?.id ?? null)),
    ) ?? [];
  const visible =
    page?.items.filter(
      (role) =>
        (!picker || (role.version > 0 && role.id.startsWith(prefix))) &&
        role.id !== exclude &&
        !role.path_ids.includes(exclude ?? ""),
    ) ?? [];
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="space-y-3 border-b px-4 py-3">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <span className="text-xs font-medium">
            {picker
              ? "親ロールを選ぶ"
              : `${query ? "検索結果" : "この階層"} ${page?.total.toLocaleString("ja-JP") ?? "—"}件`}
          </span>
          {onSave && (
            <Button
              size="sm"
              variant="outline"
              disabled={disabled}
              onClick={() => setEditor({})}
            >
              <Plus aria-hidden />
              ロールを作成
            </Button>
          )}
        </div>
        <IconInput
          icon={Search}
          type="search"
          aria-label={picker ? "親ロールを検索" : "ロールを検索"}
          value={query}
          onValueChange={(value) => {
            setQuery(value);
            setCursor(null);
            setDraftPage(0);
          }}
          placeholder="名前・IDから全階層を検索"
          className="h-9"
        />
        <nav
          aria-label="ロール階層"
          className="flex flex-wrap items-center gap-1 text-xs"
        >
          <button
            onClick={() => navigate(null)}
            className="rounded px-1 py-1 text-muted-foreground hover:bg-accent"
          >
            最上位
          </button>
          {parent && (
            <>
              <ChevronRight
                className="size-3 text-muted-foreground"
                aria-hidden
              />
              <span className="min-w-0 truncate" title={rolePath(parent)}>
                {rolePath(parent)}
              </span>
            </>
          )}
        </nav>
      </div>
      {!!selected?.length && !picker && (
        <div
          className="flex max-h-28 flex-wrap gap-1 overflow-auto border-b px-4 py-2"
          aria-label="選択したロール"
        >
          {selected.map((id) => (
            <button
              key={id}
              disabled={disabled}
              onClick={() => change(id, false)}
              title={known[id] ? rolePath(known[id]) : id}
              className="max-w-full truncate rounded border bg-muted/30 px-2 py-1 text-[11px] hover:bg-accent"
            >
              {draftIndex.get(id)?.label ?? known[id]?.label ?? id} ×
            </button>
          ))}
        </div>
      )}
      <div
        className="min-h-0 flex-1 overflow-y-auto overscroll-contain p-2"
        aria-busy={loading}
      >
        {error && (
          <p role="alert" className="p-3 text-xs text-destructive">
            {error}
          </p>
        )}
        {loading ? (
          <p role="status" className="p-3 text-xs text-muted-foreground">
            ロールを取得中…
          </p>
        ) : (
          <>
            {pendingRows
              .slice(draftPage * 40, (draftPage + 1) * 40)
              .map((row) => {
                const role = draftView(row);
                return (
                  <div
                    key={role.id}
                    className="flex items-center gap-2 rounded-md px-2 py-2 text-[13px] hover:bg-accent/50"
                  >
                    {onChange ? (
                      <input
                        type={picker ? "radio" : "checkbox"}
                        name={picker ? "parent-role" : undefined}
                        aria-label={`${role.label}を${picker ? "親にする" : "対応付ける"}`}
                        disabled={disabled}
                        checked={selected?.includes(role.id) ?? false}
                        onChange={(e) =>
                          picker
                            ? onChange([role.id])
                            : change(role.id, e.target.checked)
                        }
                        className="size-3.5 shrink-0 accent-primary"
                      />
                    ) : (
                      <Folder
                        className="size-4 shrink-0 text-muted-foreground"
                        aria-hidden
                      />
                    )}
                    <span className="min-w-0 flex-1 truncate">
                      {role.label}
                    </span>
                    <span className="shrink-0 text-[10px] text-muted-foreground">
                      新規・未反映
                    </span>
                    {onSave && (
                      <Button
                        variant="ghost"
                        size="icon"
                        className="size-8 shrink-0"
                        aria-label={`${role.label}を編集`}
                        disabled={disabled}
                        onClick={() => setEditor({ role })}
                      >
                        <Pencil className="size-3.5" />
                      </Button>
                    )}
                    {role.child_count > 0 && (
                      <Button
                        size="sm"
                        variant="ghost"
                        aria-label={`${role.label}の配下を開く`}
                        onClick={() => navigate(role)}
                      >
                        <span className="text-[11px]">{role.child_count}</span>
                        <ChevronRight className="size-3" aria-hidden />
                      </Button>
                    )}
                  </div>
                );
              })}
            {pendingRows.length > 40 && (
              <div className="flex justify-end gap-2 px-2 py-1">
                <span className="mr-auto text-[11px] text-muted-foreground">
                  未反映 {pendingRows.length}件
                </span>
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={!draftPage}
                  onClick={() => setDraftPage(draftPage - 1)}
                >
                  前の案
                </Button>
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={(draftPage + 1) * 40 >= pendingRows.length}
                  onClick={() => setDraftPage(draftPage + 1)}
                >
                  次の案
                </Button>
              </div>
            )}
            <ul aria-label={picker ? "親ロール候補" : "ロール一覧"}>
              {visible.map((role) => (
                <li
                  key={role.id}
                  className="flex items-center gap-1 rounded-md px-2 py-2 hover:bg-accent/50"
                >
                  {onChange && (
                    <input
                      type={picker ? "radio" : "checkbox"}
                      name={picker ? "parent-role" : undefined}
                      aria-label={`${role.label}を${picker ? "親にする" : "対応付ける"}`}
                      disabled={
                        disabled ||
                        role.version === 0 ||
                        !role.id.startsWith(prefix)
                      }
                      checked={selected?.includes(role.id) ?? false}
                      onChange={(e) =>
                        picker
                          ? onChange([role.id])
                          : change(role.id, e.target.checked)
                      }
                      className="size-3.5 shrink-0 accent-primary"
                    />
                  )}
                  <div className="min-w-0 flex-1">
                    <p className="truncate text-[13px] font-medium">
                      {draftIndex.get(role.id)?.label ?? role.label}
                      {draftIndex.has(role.id) && (
                        <span className="ml-2 text-[10px] font-normal text-muted-foreground">
                          変更あり
                        </span>
                      )}
                    </p>
                    <p
                      className="mt-0.5 truncate text-[11px] text-muted-foreground"
                      title={rolePath(role)}
                    >
                      {rolePath(role)}
                    </p>
                  </div>
                  {onSave && (
                    <Button
                      variant="ghost"
                      size="icon"
                      className="size-8 shrink-0"
                      aria-label={`${role.label}を編集`}
                      disabled={
                        disabled ||
                        role.version === 0 ||
                        !role.id.startsWith(prefix)
                      }
                      onClick={() =>
                        setEditor({
                          role: draftIndex.has(role.id)
                            ? {
                                ...role,
                                label: draftIndex.get(role.id)!.label,
                                parent_id: draftIndex.get(role.id)!.parent_id,
                              }
                            : role,
                        })
                      }
                    >
                      <Pencil className="size-3.5" />
                    </Button>
                  )}
                  {(role.version === 0 || !role.id.startsWith(prefix)) && (
                    <span className="shrink-0 text-[10px] text-muted-foreground">
                      IdPで管理
                    </span>
                  )}
                  {role.child_count > 0 && (
                    <Button
                      variant="ghost"
                      size="sm"
                      aria-label={`${role.label}の配下を開く`}
                      onClick={() => navigate(role)}
                    >
                      <Folder className="size-3.5" aria-hidden />
                      <span className="text-[11px] tabular-nums">
                        {role.child_count}
                      </span>
                      <ChevronRight className="size-3" aria-hidden />
                    </Button>
                  )}
                </li>
              ))}
            </ul>
            {!visible.length && (
              <p className="p-3 text-xs text-muted-foreground">
                該当するロールはありません。
              </p>
            )}
          </>
        )}
      </div>
      <div className="flex min-h-12 items-center justify-between gap-2 border-t px-4 py-2 text-[11px] text-muted-foreground">
        <span>{page?.items.length ?? 0}件を表示</span>
        <div className="flex gap-2">
          {cursor && (
            <Button size="sm" variant="ghost" onClick={() => setCursor(null)}>
              先頭
            </Button>
          )}
          {page?.next_cursor && (
            <Button
              size="sm"
              variant="outline"
              onClick={() => setCursor(page.next_cursor ?? null)}
            >
              次へ
            </Button>
          )}
        </div>
      </div>
      {editor && onSave && (
        <RoleEditor
          role={editor.role}
          parent={parent}
          prefix={prefix}
          revision={revision}
          assignAllowed={!!onChange}
          drafts={drafts}
          onClose={() => setEditor(undefined)}
          onSave={(value, assign) => {
            onSave(value, assign);
            setEditor(undefined);
          }}
        />
      )}
    </div>
  );
}
function RoleEditor({
  role,
  parent,
  prefix,
  revision,
  assignAllowed,
  drafts,
  onClose,
  onSave,
}: {
  role?: Role;
  parent: Role | null;
  prefix: string;
  revision: number;
  assignAllowed: boolean;
  drafts?: RoleEdit[];
  onClose: () => void;
  onSave: (role: RoleEdit, assign: boolean) => void;
}) {
  const [label, setLabel] = React.useState(role?.label ?? "");
  const [parentId, setParentId] = React.useState(
    role ? (role.parent_id ?? null) : (parent?.id ?? null),
  );
  const [parentLabel, setParentLabel] = React.useState("最上位");
  const [parentError, setParentError] = React.useState("");
  React.useEffect(() => {
    if (!parentId) {
      setParentLabel("最上位");
      setParentError("");
      return;
    }
    const pending = drafts?.find((r) => r.id === parentId);
    if (pending) {
      setParentLabel(pending.label);
      setParentError("");
      return;
    }
    const controller = new AbortController();
    setParentLabel("親ロールを取得中…");
    setParentError("");
    request<RolePage>(`/roles${params({ ids: parentId })}`, {
      signal: controller.signal,
    })
      .then((page) => {
        setParentLabel(
          page.items[0] ? rolePath(page.items[0]) : "親ロールが見つかりません",
        );
      })
      .catch((e) => {
        if (!controller.signal.aborted) {
          setParentLabel("選択した親ロール");
          setParentError(e.message);
        }
      });
    return () => controller.abort();
  }, [parentId, drafts]);
  const [choosing, setChoosing] = React.useState(false);
  const [assign, setAssign] = React.useState(assignAllowed && !role);
  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open) onClose();
      }}
    >
      <DialogContent className="flex max-h-[90dvh] max-w-xl flex-col overflow-hidden">
        <DialogTitle>{role ? "ロールを編集" : "ロールを作成"}</DialogTitle>
        <DialogDescription>
          名前や親を変更してもIDは変わりません。変更は確認後に反映します。
        </DialogDescription>
        {role && (
          <details className="text-xs text-muted-foreground">
            <summary className="cursor-pointer">ロールID（CSV用）</summary>
            <p className="mt-2 select-all break-all font-mono text-[11px]">
              {role.id}
            </p>
          </details>
        )}
        <label className="space-y-2 text-xs font-medium">
          ロール名
          <Input
            autoFocus
            maxLength={80}
            value={label}
            onChange={(e) => setLabel(e.target.value)}
            placeholder="ロール名"
          />
        </label>
        <div className="space-y-2">
          <p className="text-xs font-medium">親ロール</p>
          <div className="flex items-center gap-2">
            <span
              className="min-w-0 flex-1 truncate text-xs text-muted-foreground"
              title={parentLabel}
            >
              {parentLabel}
            </span>
            <Button
              size="sm"
              variant="outline"
              onClick={() => setChoosing(!choosing)}
            >
              親を選ぶ
            </Button>
            {parentId && (
              <Button
                size="sm"
                variant="ghost"
                onClick={() => setParentId(null)}
              >
                最上位にする
              </Button>
            )}
          </div>
        </div>
        {parentError && (
          <p role="alert" className="text-xs text-destructive">
            {parentError}
          </p>
        )}
        {choosing && (
          <div
            className="flex min-h-0 flex-1 flex-col overflow-hidden rounded-md border"
            style={{ height: 300 }}
          >
            <Roles
              picker
              drafts={drafts}
              revision={revision}
              prefix={prefix}
              exclude={role?.id}
              selected={parentId ? [parentId] : []}
              onChange={([id]) => {
                setParentId(id);
                setChoosing(false);
              }}
            />
          </div>
        )}
        {!role && assignAllowed && (
          <label className="flex items-center gap-2 text-xs">
            <input
              type="checkbox"
              checked={assign}
              onChange={(e) => setAssign(e.target.checked)}
            />
            作成後、このADグループに対応付ける
          </label>
        )}
        <div className="flex justify-end gap-2">
          <Button variant="outline" onClick={onClose}>
            キャンセル
          </Button>
          <Button
            disabled={!label.trim()}
            onClick={() =>
              onSave(
                {
                  id: role?.id ?? `${prefix}${crypto.randomUUID()}`,
                  label: label.trim(),
                  parent_id: parentId,
                  expected_version: role?.version ?? 0,
                },
                assign,
              )
            }
          >
            編集内容に追加
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}
