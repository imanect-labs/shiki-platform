"use client";

import * as React from "react";
import { ChevronRight, Search, UserPlus } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { IconInput } from "@/components/ui/icon-input";
import { SegmentedControl } from "@/components/ui/segmented-control";
import { Sheet, SheetBody, SheetContent, SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet";
import { ChromeIconButton } from "@/components/shell/page-header-context";
import { useMediaQuery } from "@/hooks/use-media-query";
import { cn } from "@/lib/utils";
import { AdminPage, Panel } from "./admin-page";
import { MockBanner } from "./mock-banner";

type Member = {
  id: string;
  name: string;
  email: string;
  role: string;
  status: "active" | "invited" | "disabled";
  last: string;
};

/// サンプル（API は Task 8.4 で実装）。実在の人物ではない。
const MEMBERS: Member[] = [
  { id: "u_7f3a…c21e", name: "佐藤 美咲", email: "sato.m@example.co.jp", role: "営業1課", status: "active", last: "10/09 10:24" },
  { id: "u_1b08…9a4d", name: "小林 大輔", email: "kobayashi@example.co.jp", role: "営業部", status: "active", last: "10/09 09:02" },
  { id: "u_c4e2…0b77", name: "高橋 直樹", email: "takahashi@example.co.jp", role: "営業2課", status: "active", last: "10/06 17:45" },
  { id: "u_93dd…e510", name: "渡辺 彩", email: "watanabe@example.co.jp", role: "営業1課", status: "invited", last: "—" },
  { id: "u_5a71…3f02", name: "鈴木 健太", email: "suzuki@example.co.jp", role: "開発部", status: "active", last: "10/08 13:11" },
  { id: "u_e810…77c9", name: "中村 誠", email: "nakamura@example.co.jp", role: "経理部", status: "disabled", last: "8/02 18:40" },
];

/// 状態は文字のバッジで示す（有効=淡い緑・招待中=淡い琥珀・無効=淡い灰。ドットは使わない）。
const STATUS = {
  active: { label: "有効", variant: "success" },
  invited: { label: "招待中", variant: "warning" },
  disabled: { label: "無効", variant: "muted" },
} as const;

const FILTERS = [
  { value: "all", label: "すべて" },
  { value: "active", label: "有効" },
  { value: "invited", label: "招待中" },
  { value: "disabled", label: "無効" },
];

/// メンバー一覧（モック）。行を選ぶと詳細とアクセスの理由をシートに出す
/// （PC は右から・スマホは下から）。スマホは表をやめて 1 人 1 行のカードにする。
export function MockMembersView() {
  const [filter, setFilter] = React.useState("all");
  const [query, setQuery] = React.useState("");
  const [selected, setSelected] = React.useState<Member | null>(null);
  const desktop = useMediaQuery("(min-width: 768px)");
  const rows = MEMBERS.filter(
    (m) =>
      (filter === "all" || m.status === filter) &&
      (query === "" || m.name.includes(query) || m.email.includes(query)),
  );

  return (
    <AdminPage
      title="メンバー"
      description="組織のメンバーと所属ロール。"
      wide
      actions={
        <Button size="sm" disabled>
          <UserPlus aria-hidden />
          メンバーを招待
        </Button>
      }
      mobileActions={
        <ChromeIconButton label="メンバーを招待（準備中）" disabled className="disabled:opacity-40">
          <UserPlus aria-hidden />
        </ChromeIconButton>
      }
    >
      <MockBanner what="メンバー管理の API は Task 8.4 で実装予定" />
      <div className="flex flex-col gap-3 md:flex-row md:flex-wrap md:items-center md:gap-2">
        <IconInput
          icon={Search}
          type="search"
          enterKeyHint="search"
          value={query}
          onValueChange={setQuery}
          placeholder="名前・メールで検索"
          aria-label="メンバーを検索"
          className="max-md:h-12 max-md:rounded-xl"
          containerClassName="w-full md:w-64"
        />
        <SegmentedControl
          aria-label="状態で絞り込む"
          size="sm"
          value={filter}
          onValueChange={setFilter}
          options={FILTERS}
          className="max-md:hidden"
        />
        {/* スマホ: 横に並ぶチップ（はみ出したら横スクロール）。 */}
        <div
          role="radiogroup"
          aria-label="状態で絞り込む"
          className="-mx-4 flex gap-2 overflow-x-auto overscroll-x-contain px-4 md:hidden"
        >
          {FILTERS.map((f) => {
            const active = filter === f.value;
            return (
              <button
                key={f.value}
                type="button"
                role="radio"
                aria-checked={active}
                onClick={() => setFilter(f.value)}
                className={cn(
                  "h-10 shrink-0 rounded-full border px-4 text-sm outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring",
                  active
                    ? "border-transparent bg-accent font-medium text-accent-foreground"
                    : "bg-background text-muted-foreground",
                )}
              >
                {f.label}
              </button>
            );
          })}
        </div>
      </div>

      {/* スマホ: 1 人 1 行のカード（行全体をタップで詳細）。 */}
      <ul className="rule-soft overflow-hidden rounded-xl border bg-card md:hidden" aria-label="メンバー">
        {rows.length === 0 ? (
          <li className="px-4 py-8 text-center text-sm text-muted-foreground">該当するメンバーはいません</li>
        ) : null}
        {rows.map((m, i) => (
          <li key={m.id} className={cn(i > 0 && "shiki-dash-top")}>
            <button
              type="button"
              onClick={() => setSelected(m)}
              className="flex min-h-[72px] w-full items-center gap-3 px-4 py-3 text-left outline-none transition-colors active:bg-accent focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
            >
              <span
                aria-hidden
                className="flex size-10 shrink-0 items-center justify-center rounded-full bg-muted text-sm font-semibold text-foreground/80"
              >
                {m.name.slice(0, 1)}
              </span>
              <span className="min-w-0 flex-1">
                <span className="block truncate text-[15px] font-medium">{m.name}</span>
                <span className="block truncate text-xs text-muted-foreground">
                  {m.role}
                  <span className="mx-1.5" />
                  {m.email}
                </span>
              </span>
              <Badge variant={STATUS[m.status].variant} className="shrink-0">
                {STATUS[m.status].label}
              </Badge>
              <ChevronRight className="size-4 shrink-0 text-muted-foreground" aria-hidden />
            </button>
          </li>
        ))}
      </ul>

      <Panel className="max-md:hidden">
        <div className="overflow-x-auto">
          <table className="w-full min-w-[760px] border-collapse text-[13px]">
            <thead>
              <tr className="bg-muted/40 text-left text-xs text-muted-foreground">
                <th scope="col" className="px-4 py-2 font-medium">表示名</th>
                <th scope="col" className="px-2 py-2 font-medium">メール</th>
                <th scope="col" className="px-2 py-2 font-medium">ロール</th>
                <th scope="col" className="px-2 py-2 font-medium">状態</th>
                <th scope="col" className="px-2 py-2 font-medium">最終ログイン</th>
                <th scope="col" className="px-4 py-2 font-medium">ユーザー ID</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((m) => (
                <tr key={m.id} className="border-t hover:bg-accent/40">
                  <td className="px-4 py-2.5">
                    <button
                      type="button"
                      onClick={() => setSelected(m)}
                      className="font-medium outline-none hover:underline focus-visible:ring-2 focus-visible:ring-ring"
                    >
                      {m.name}
                    </button>
                  </td>
                  <td className="px-2 py-2.5 text-foreground/80">{m.email}</td>
                  <td className="px-2 py-2.5">{m.role}</td>
                  <td className="px-2 py-2.5">
                    <Badge variant={STATUS[m.status].variant}>{STATUS[m.status].label}</Badge>
                  </td>
                  <td className="px-2 py-2.5 text-muted-foreground">{m.last}</td>
                  <td className="px-4 py-2.5 font-mono text-xs text-muted-foreground">{m.id}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </Panel>

      <Sheet open={!!selected} onOpenChange={(o) => !o && setSelected(null)}>
        <SheetContent side={desktop ? "right" : "bottom"} className="md:max-w-md">
          {selected ? (
            <>
              <SheetHeader>
                <SheetTitle className="text-base font-semibold">{selected.name}</SheetTitle>
                <SheetDescription className="text-xs text-muted-foreground">{selected.email}</SheetDescription>
              </SheetHeader>
              <SheetBody className="flex flex-col gap-4 text-[13px]">
                <div>
                  <div className="mb-1.5 text-xs text-muted-foreground">アクセスの理由（例: 2026下期 見積書.xlsx）</div>
                  <ol className="flex flex-col gap-2 rounded-lg border bg-muted/30 p-3">
                    <li>{selected.name}<span className="ml-2 text-xs text-muted-foreground">ユーザー</span></li>
                    <li>{selected.role} のメンバー<span className="ml-2 text-xs text-muted-foreground">直接付与</span></li>
                    <li>フォルダ「営業資料」の閲覧者<span className="ml-2 font-mono text-xs text-muted-foreground">viewer ← role#member</span></li>
                    <li>2026下期 見積書.xlsx<span className="ml-2 text-xs text-muted-foreground">親フォルダから継承</span></li>
                  </ol>
                </div>
                <div className="rounded-lg border border-destructive/40 p-3">
                  <div className="font-medium text-destructive">アカウントの無効化</div>
                  <p className="mt-1 text-xs text-muted-foreground">無効化すると即座にログインできなくなります。</p>
                  <Button variant="outline" size="sm" className="mt-2" disabled>
                    無効化
                  </Button>
                </div>
              </SheetBody>
            </>
          ) : null}
        </SheetContent>
      </Sheet>
    </AdminPage>
  );
}
