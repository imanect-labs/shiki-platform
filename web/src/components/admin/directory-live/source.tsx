"use client";
import * as React from "react";
import { ChevronRight, Folder, Search, Users } from "lucide-react";
import { Button } from "@/components/ui/button";
import { IconInput } from "@/components/ui/icon-input";
import { cn } from "@/lib/utils";
import {
  request,
  params,
  type GroupPage,
  type FolderPage,
  type Group,
} from "./api";

/** OUの各階層をサーバで取得する。検索は全OUを対象にし、全件は保持しない。 */
export function Source({
  revision,
  selected,
  onSelect,
  checked,
  onCheck,
  disabled = false,
}: {
  revision: number;
  selected?: string;
  onSelect: (group: Group) => void;
  checked?: string[];
  onCheck?: (group: Group, checked: boolean) => void;
  disabled?: boolean;
}) {
  const [ou, setOu] = React.useState<string[]>([]);
  const [query, setQuery] = React.useState("");
  const [cursor, setCursor] = React.useState<string | null>(null);
  const [folderCursor, setFolderCursor] = React.useState<string | null>(null);
  const [groups, setGroups] = React.useState<GroupPage>();
  const [folders, setFolders] = React.useState<FolderPage>();
  const [error, setError] = React.useState("");
  const [loading, setLoading] = React.useState(true);
  const key = JSON.stringify(ou);
  React.useEffect(() => {
    const controller = new AbortController();
    setLoading(true);
    setError("");
    const timer = setTimeout(
      () => {
        Promise.all([
          request<GroupPage>(
            `/groups${params({ q: query, parent: key, cursor })}`,
            { signal: controller.signal },
          ),
          request<FolderPage>(
            `/folders${params({ parent: key, cursor: folderCursor })}`,
            { signal: controller.signal },
          ),
        ])
          .then(([g, f]) => {
            setGroups(g);
            setFolders(f);
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
  }, [key, query, cursor, folderCursor, revision]);
  const navigate = (path: string[]) => {
    setOu(path);
    setCursor(null);
    setFolderCursor(null);
  };
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="space-y-3 border-b px-4 py-3">
        <div className="flex items-center justify-between">
          <h2 className="text-sm font-semibold">ADグループ</h2>
          <span className="text-xs text-muted-foreground">
            {groups?.total.toLocaleString("ja-JP") ?? "—"}件
          </span>
        </div>
        <IconInput
          icon={Search}
          type="search"
          aria-label="ADグループを検索"
          value={query}
          onValueChange={(value) => {
            setQuery(value);
            setCursor(null);
          }}
          placeholder="全OUから名前・OU名で検索"
          className="h-9"
        />
        <nav
          aria-label="ADのOU階層"
          className="flex flex-wrap items-center gap-1 text-xs"
        >
          <button
            onClick={() => navigate([])}
            className="rounded px-1 py-1 text-muted-foreground hover:bg-accent"
          >
            すべて
          </button>
          {ou.map((name, index) => (
            <React.Fragment key={index}>
              <ChevronRight
                className="size-3 text-muted-foreground"
                aria-hidden
              />
              <button
                className="max-w-32 truncate rounded px-1 py-1 hover:bg-accent"
                title={name}
                onClick={() => navigate(ou.slice(0, index + 1))}
              >
                {name}
              </button>
            </React.Fragment>
          ))}
        </nav>
      </div>
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
            グループを取得中…
          </p>
        ) : (
          <>
            {!query &&
              folders?.items.map((folder) => (
                <button
                  key={folder.name}
                  onClick={() => navigate([...ou, folder.name])}
                  className="flex w-full items-center gap-2 rounded-md px-2 py-2.5 text-left text-[13px] hover:bg-accent"
                >
                  <Folder
                    className="size-4 shrink-0 text-muted-foreground"
                    aria-hidden
                  />
                  <span className="min-w-0 flex-1 truncate">{folder.name}</span>
                  <span className="text-[11px] tabular-nums text-muted-foreground">
                    {folder.group_count}
                  </span>
                  <ChevronRight
                    className="size-3 text-muted-foreground"
                    aria-hidden
                  />
                </button>
              ))}
            {!query && folders?.next_cursor && (
              <Button
                variant="ghost"
                size="sm"
                className="w-full"
                onClick={() => setFolderCursor(folders.next_cursor ?? null)}
              >
                次のOU
              </Button>
            )}
            <ul aria-label="ADグループ一覧">
              {groups?.items.map((group) => (
                <li
                  key={group.id}
                  className={cn(
                    "flex items-center gap-1 rounded-md",
                    selected === group.id && "bg-accent",
                  )}
                >
                  {onCheck && (
                    <input
                      type="checkbox"
                      disabled={disabled}
                      aria-label={`${group.name}を選択`}
                      checked={checked?.includes(group.id) ?? false}
                      onChange={(e) => onCheck(group, e.target.checked)}
                      className="ml-2 size-3.5 shrink-0 accent-primary"
                    />
                  )}
                  <button
                    onClick={() => onSelect(group)}
                    aria-pressed={selected === group.id}
                    className="flex min-w-0 flex-1 items-start gap-2 rounded-md px-2 py-2.5 text-left hover:bg-accent/60"
                  >
                    <Users
                      className="mt-0.5 size-4 shrink-0 text-muted-foreground"
                      aria-hidden
                    />
                    <span className="min-w-0 flex-1">
                      <span className="block truncate text-[13px] font-medium">
                        {group.name}
                      </span>
                      <span className="mt-0.5 block truncate text-[11px] text-muted-foreground">
                        {query
                          ? group.ou_path.join(" / ") || "OUなし"
                          : group.role_ids.length
                            ? `${group.role_ids.length}ロールに対応`
                            : "未対応"}
                      </span>
                    </span>
                  </button>
                </li>
              ))}
            </ul>
            {!groups?.items.length && (query || !folders?.items.length) && (
              <p className="p-3 text-xs leading-6 text-muted-foreground">
                {groups?.total
                  ? "このOUに直接配置されたグループはありません。"
                  : "グループがありません。接続設定を保存し、ADを同期してください。"}
              </p>
            )}
            <div className="flex justify-between gap-2 pt-2">
              {cursor && (
                <Button
                  size="sm"
                  variant="ghost"
                  onClick={() => setCursor(null)}
                >
                  先頭に戻る
                </Button>
              )}
              {groups?.next_cursor && (
                <Button
                  size="sm"
                  variant="outline"
                  className="ml-auto"
                  onClick={() => setCursor(groups.next_cursor ?? null)}
                >
                  次のグループ
                </Button>
              )}
            </div>
          </>
        )}
      </div>
      <p className="border-t px-4 py-2.5 text-[11px] leading-5 text-muted-foreground">
        OUは配置です。配下にあるだけではロールは付与されません。
      </p>
    </div>
  );
}
