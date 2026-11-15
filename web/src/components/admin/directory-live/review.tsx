"use client";
import * as React from "react";
import {
  ChevronDown,
  ChevronRight,
  Folder,
  Plus,
  Search,
  UsersRound,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { IconInput } from "@/components/ui/icon-input";
import {
  request,
  params,
  type Role,
  type RoleEdit,
  type Changes,
  type Group,
  type RolePage,
} from "./api";
import { cn } from "@/lib/utils";

export function RoleTree({
  nodes,
  changes,
  ouPaths,
}: {
  nodes: RoleEdit[];
  changes: Changes;
  ouPaths?: Set<string>;
}) {
  const [query, setQuery] = React.useState("");
  const [expanded, setExpanded] = React.useState<Set<string>>(
    () => new Set(nodes.length < 100 ? nodes.map((r) => r.id) : []),
  );
  const [limits, setLimits] = React.useState<Record<string, number>>({});
  const fresh = new Set(
    changes.roles.filter((r) => r.expected_version === 0).map((r) => r.id),
  );
  const updated = new Set(
    changes.roles.filter((r) => r.expected_version > 0).map((r) => r.id),
  );
  const index = new Map(nodes.map((r) => [r.id, r]));
  const children = new Map<string | null, RoleEdit[]>();
  nodes.forEach((role) => {
    const parent =
      role.parent_id && index.has(role.parent_id) ? role.parent_id : null;
    const list = children.get(parent) ?? [];
    list.push(role);
    children.set(parent, list);
  });
  const labelPath = (id: string, separator = " / ") => {
    const labels = [];
    const visited = new Set();
    let role = index.get(id);
    while (role && !visited.has(role.id)) {
      visited.add(role.id);
      labels.unshift(role.label);
      role = role.parent_id ? index.get(role.parent_id) : undefined;
    }
    return labels.join(separator);
  };
  const branch = (parent: string | null, depth: number): React.ReactNode => {
    if (depth > 128) return null;
    const list = children.get(parent) ?? [];
    const limit = limits[parent ?? "root"] ?? 40;
    return (
      <>
        {list.slice(0, limit).map((role) => {
          const count = children.get(role.id)?.length ?? 0;
          const open = expanded.has(role.id);
          const fromOu = ouPaths?.has(labelPath(role.id, "\0"));
          const Icon = ouPaths && !fromOu ? UsersRound : Folder;
          return (
            <React.Fragment key={role.id}>
              <div
                role="treeitem"
                aria-selected={false}
                aria-level={depth + 1}
                aria-expanded={count ? open : undefined}
                className="flex min-w-0 items-center gap-2 py-2 pr-3 text-[13px]"
                style={{ paddingLeft: 12 + depth * 18 }}
              >
                {count ? (
                  <button
                    aria-label={`${role.label}の階層を${open ? "閉じる" : "開く"}`}
                    onClick={() =>
                      setExpanded((old) => {
                        const next = new Set(old);
                        if (next.has(role.id)) next.delete(role.id);
                        else next.add(role.id);
                        return next;
                      })
                    }
                  >
                    {open ? (
                      <ChevronDown className="size-3.5" />
                    ) : (
                      <ChevronRight className="size-3.5" />
                    )}
                  </button>
                ) : (
                  <span className="w-3.5 shrink-0" />
                )}
                <Icon
                  className="size-4 shrink-0 text-muted-foreground"
                  aria-hidden
                />
                <span
                  className="min-w-0 flex-1 truncate"
                  title={labelPath(role.id)}
                >
                  {role.label}
                </span>
                {ouPaths && (
                  <span className="shrink-0 text-[10px] text-muted-foreground">
                    {fromOu ? "OU由来" : "グループ由来"}
                  </span>
                )}
                <span
                  className={cn(
                    "shrink-0 text-[10px]",
                    fresh.has(role.id) || updated.has(role.id)
                      ? "text-foreground"
                      : "text-muted-foreground",
                  )}
                >
                  {fresh.has(role.id)
                    ? "新規"
                    : updated.has(role.id)
                      ? "変更"
                      : "既存を使用"}
                </span>
              </div>
              {count > 0 && open && (
                <div role="group">{branch(role.id, depth + 1)}</div>
              )}
            </React.Fragment>
          );
        })}
        {list.length > limit && (
          <button
            className="px-6 py-2 text-xs text-muted-foreground hover:text-foreground"
            onClick={() =>
              setLimits((old) => ({ ...old, [parent ?? "root"]: limit + 40 }))
            }
          >
            さらに40件表示（残り{list.length - limit}件）
          </button>
        )}
      </>
    );
  };
  const matches = nodes.filter((r) =>
    r.label.toLowerCase().includes(query.toLowerCase()),
  );
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex flex-wrap items-center justify-between gap-2 shiki-dash-bottom px-4 py-3">
        <h3 className="text-xs font-semibold">反映後のロール階層</h3>
        <span className="text-[11px] text-muted-foreground">
          新規 {fresh.size} / 変更 {updated.size} / 既存{" "}
          {nodes.length - fresh.size - updated.size}
        </span>
        <IconInput
          icon={Search}
          aria-label="作成案を検索"
          value={query}
          onValueChange={setQuery}
          placeholder="ロール名で検索"
          className="h-8 text-xs"
        />
      </div>
      <div
        className="min-h-0 flex-1 overflow-auto py-2"
        role="tree"
        aria-label="作成するロール階層"
      >
        {query
          ? matches.slice(0, 100).map((r) => (
              <div key={r.id} className="px-4 py-2 text-xs">
                <p className="font-medium">{r.label}</p>
                <p className="mt-1 truncate text-muted-foreground">
                  {labelPath(r.id)}
                </p>
              </div>
            ))
          : branch(null, 0)}
        {!nodes.length && (
          <p className="px-4 py-3 text-xs leading-6 text-muted-foreground">
            対応表だけを変更します。ロールの新規作成・階層の変更はありません。
          </p>
        )}
        {query && matches.length > 100 && (
          <p className="p-4 text-xs text-muted-foreground">
            先頭100件を表示しています。検索を絞ってください。
          </p>
        )}
      </div>
    </div>
  );
}

export function MappingDiff({
  group,
  changes,
  nodes,
  beforeRoles,
}: {
  group?: Group;
  changes: Changes;
  nodes: RoleEdit[];
  beforeRoles: Role[];
}) {
  const [labelError, setLabelError] = React.useState(false);
  const [labels, setLabels] = React.useState<Record<string, string>>({});
  const mapping = changes.mappings.find((m) => m.group_id === group?.id);
  const ids = [
    ...new Set([...(group?.role_ids ?? []), ...(mapping?.role_ids ?? [])]),
  ];
  const savedLabels = new Map(
    beforeRoles.map((r) => [r.id, r.path_labels.join(" / ") || r.label]),
  );
  const nodeIds = new Set(nodes.map((r) => r.id));
  const key = ids
    .filter(
      (id) =>
        !savedLabels.has(id) &&
        (!nodeIds.has(id) || group?.role_ids.includes(id)),
    )
    .sort()
    .join(",");
  React.useEffect(() => {
    const controller = new AbortController();
    (async () => {
      const ids = key ? key.split(",") : [];
      for (let i = 0; i < ids.length; i += 100) {
        const roles = await request<RolePage>(
          `/roles${params({ ids: ids.slice(i, i + 100).join(",") })}`,
          { signal: controller.signal },
        );
        setLabels((old) => ({
          ...old,
          ...Object.fromEntries(
            roles.items.map((r) => [
              r.id,
              r.path_labels.join(" / ") || r.label,
            ]),
          ),
        }));
      }
    })()
      .then(() => {
        if (!controller.signal.aborted) setLabelError(false);
      })
      .catch(() => {
        if (!controller.signal.aborted) setLabelError(true);
      });
    return () => controller.abort();
  }, [key]);
  const index = new Map(nodes.map((r) => [r.id, r]));
  const name = (id: string) => {
    const path = [];
    const visited = new Set();
    let r = index.get(id);
    while (r && !visited.has(r.id)) {
      visited.add(r.id);
      path.unshift(r.label);
      r = r.parent_id ? index.get(r.parent_id) : undefined;
    }
    return path.length ? path.join(" / ") : (labels[id] ?? id);
  };
  const before = group?.role_ids ?? [];
  const after = mapping?.role_ids ?? before;
  return (
    <div className="shrink-0 shiki-dash-top bg-muted/10 px-4 py-3 text-xs">
      <p className="mb-2 truncate font-semibold">
        {group
          ? `${group.name} の対応付け`
          : "グループを選ぶと対応付けを確認できます"}
      </p>
      {labelError && (
        <p role="alert" className="mb-2 text-[11px] text-destructive">
          表示名を取得できないロールはIDで表示しています。接続を確認して再読込してください。
        </p>
      )}
      {group && (
        <div className="grid gap-3 sm:grid-cols-2">
          <div>
            <p className="text-[10px] text-muted-foreground">現在</p>
            <ul className="mt-1 max-h-24 overflow-auto space-y-1">
              {before.length ? (
                before.map((id) => (
                  <li key={id} className="break-words">
                    {savedLabels.get(id) ?? labels[id] ?? id}
                  </li>
                ))
              ) : (
                <li className="text-muted-foreground">対応なし</li>
              )}
            </ul>
          </div>
          <div>
            <p className="text-[10px] text-muted-foreground">反映後</p>
            <ul className="mt-1 max-h-24 overflow-auto space-y-1">
              {after.length ? (
                after.map((id) => (
                  <li key={id} className="flex items-start gap-1 break-words">
                    {!before.includes(id) && (
                      <Plus
                        className="mt-0.5 size-3 shrink-0"
                        aria-label="追加"
                      />
                    )}
                    <span>{name(id)}</span>
                  </li>
                ))
              ) : (
                <li className="text-muted-foreground">対応を解除</li>
              )}
            </ul>
          </div>
        </div>
      )}
    </div>
  );
}

export function ReviewSources({
  sources,
  active,
  onSelect,
}: {
  sources: Group[];
  active?: string;
  onSelect: (group: Group) => void;
}) {
  const [query, setQuery] = React.useState("");
  const [page, setPage] = React.useState(0);
  const filtered = sources.filter((g) =>
    `${g.name} ${g.ou_path.join("/")}`
      .toLowerCase()
      .includes(query.toLowerCase()),
  );
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="border-b px-4 py-3">
        <p className="mb-3 text-xs font-semibold">
          対象のADグループ {sources.length}件
        </p>
        <IconInput
          icon={Search}
          aria-label="確認対象を検索"
          value={query}
          onValueChange={(value) => {
            setQuery(value);
            setPage(0);
          }}
          placeholder="名前・OU名で検索"
          className="h-8 text-xs"
        />
      </div>
      <ul className="min-h-0 flex-1 overflow-auto p-2">
        {filtered.slice(page * 40, (page + 1) * 40).map((g) => (
          <li key={g.id}>
            <button
              aria-pressed={active === g.id}
              onClick={() => onSelect(g)}
              className={cn(
                "w-full rounded-md px-3 py-2 text-left hover:bg-accent",
                active === g.id && "bg-accent",
              )}
            >
              <span className="block truncate text-xs font-medium">
                {g.name}
              </span>
              <span className="mt-1 block truncate text-[10px] text-muted-foreground">
                {g.ou_path.join(" / ") || "OUなし"}
              </span>
            </button>
          </li>
        ))}
      </ul>
      <div className="flex justify-between border-t p-2">
        {page > 0 && (
          <Button size="sm" variant="ghost" onClick={() => setPage(page - 1)}>
            前へ
          </Button>
        )}
        {(page + 1) * 40 < filtered.length && (
          <Button
            className="ml-auto"
            size="sm"
            variant="ghost"
            onClick={() => setPage(page + 1)}
          >
            次へ
          </Button>
        )}
      </div>
    </div>
  );
}
