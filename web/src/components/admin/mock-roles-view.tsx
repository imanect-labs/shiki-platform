"use client";

import * as React from "react";

import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectLabel,
  SelectGroup,
  SelectSeparator,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { AdminPage, Panel, SettingRow } from "./admin-page";
import { MockBanner } from "./mock-banner";

type Role = { id: string; label: string; parent: string | null; members: number };

/// サンプルのロール階層（親ロールは配下ロールのメンバーを含む）。
const ROLES: Role[] = [
  { id: "all", label: "全社", parent: null, members: 48 },
  { id: "planning", label: "経営企画", parent: "all", members: 3 },
  { id: "sales", label: "営業部", parent: "all", members: 24 },
  { id: "sales-1", label: "営業1課", parent: "sales", members: 11 },
  { id: "sales-2", label: "営業2課", parent: "sales", members: 9 },
  { id: "dev", label: "開発部", parent: "all", members: 14 },
];

function depth(id: string): number {
  const r = ROLES.find((x) => x.id === id);
  return r?.parent ? depth(r.parent) + 1 : 0;
}

/// `id` の配下（自身を含む）。親に選ぶと循環になるロール。
function descendants(id: string): Set<string> {
  const out = new Set([id]);
  let grew = true;
  while (grew) {
    grew = false;
    for (const r of ROLES) {
      if (r.parent && out.has(r.parent) && !out.has(r.id)) {
        out.add(r.id);
        grew = true;
      }
    }
  }
  return out;
}

/// ロール階層（モック）。親ロールの候補から循環になるものを除外して見せる。
export function MockRolesView() {
  const [selected, setSelected] = React.useState("sales");
  const detailRef = React.useRef<HTMLDivElement>(null);

  /// 選ぶ。スマホ（一覧と詳細が縦に並ぶ）では詳細の先頭へスクロールして、選んだ結果を見せる。
  const choose = (id: string) => {
    setSelected(id);
    if (window.matchMedia("(max-width: 767px)").matches) {
      requestAnimationFrame(() =>
        detailRef.current?.scrollIntoView({ behavior: "smooth", block: "start" }),
      );
    }
  };
  const role = ROLES.find((r) => r.id === selected) ?? ROLES[0];
  const blocked = descendants(role.id);
  const children = ROLES.filter((r) => r.parent === role.id);

  return (
    <AdminPage title="ロール" description="テナント内の階層付きメンバーシップ。親ロールは配下ロールのメンバーを含みます。" wide>
      <MockBanner what="ロール管理の API は Task 8.4 で実装予定" />
      <div className="grid gap-4 md:grid-cols-[240px_minmax(0,1fr)]">
        <Panel>
          <ul role="tree" aria-label="ロール" className="flex flex-col p-2 text-[13px]">
            {ROLES.map((r) => (
              <li key={r.id} role="treeitem" aria-selected={r.id === selected}>
                <button
                  type="button"
                  onClick={() => choose(r.id)}
                  style={{ paddingLeft: 8 + depth(r.id) * 14 }}
                  className={cn(
                    "flex w-full items-center justify-between rounded-md py-1.5 pr-2 text-left outline-none focus-visible:ring-2 focus-visible:ring-ring pointer-coarse:min-h-11",
                    r.id === selected ? "bg-accent font-medium" : "hover:bg-accent/50",
                  )}
                >
                  {r.label}
                  <span className="font-mono text-[11px] text-muted-foreground">{r.members}</span>
                </button>
              </li>
            ))}
          </ul>
        </Panel>
        <div ref={detailRef} className="flex scroll-mt-20 flex-col gap-4">
          <h2 className="text-[15px] font-semibold md:hidden">{role.label} の設定</h2>
          <Panel title="一般">
            <SettingRow label="ロール名" htmlFor="role-name">
              <Input id="role-name" value={role.label} readOnly className="h-9 md:max-w-xs" />
            </SettingRow>
          </Panel>
          <Panel title="階層">
            <SettingRow
              label="親ロール"
              description="このロールの配下にあるロールは、循環になるため選べません。"
            >
              <Select value={role.parent ?? "none"}>
                <SelectTrigger className="h-9 w-full md:max-w-xs" aria-label="親ロール">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="none">（なし・最上位）</SelectItem>
                  {ROLES.filter((r) => !blocked.has(r.id)).map((r) => (
                    <SelectItem key={r.id} value={r.id}>
                      {r.label}
                    </SelectItem>
                  ))}
                  {blocked.size > 1 ? (
                    <>
                      <SelectSeparator />
                      <SelectGroup>
                        <SelectLabel>選べない（循環）</SelectLabel>
                        {ROLES.filter((r) => blocked.has(r.id) && r.id !== role.id).map((r) => (
                          <SelectItem key={r.id} value={r.id} disabled>
                            {r.label}
                          </SelectItem>
                        ))}
                      </SelectGroup>
                    </>
                  ) : null}
                </SelectContent>
              </Select>
            </SettingRow>
            <SettingRow label="配下ロール" description="これらのメンバーはこのロールのメンバーとして扱われます。">
              <div className="flex flex-wrap gap-1.5">
                {children.length === 0 ? (
                  <span className="text-xs text-muted-foreground">なし</span>
                ) : (
                  children.map((c) => (
                    <span key={c.id} className="rounded-md border px-2 py-0.5 text-[13px]">
                      {c.label} <span className="text-xs text-muted-foreground">{c.members} 人</span>
                    </span>
                  ))
                )}
              </div>
            </SettingRow>
          </Panel>
          <div className="flex justify-end">
            <Button size="sm" disabled>
              変更を保存
            </Button>
          </div>
        </div>
      </div>
    </AdminPage>
  );
}
