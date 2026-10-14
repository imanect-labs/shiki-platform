"use client";

import * as React from "react";
import { Check, ShieldCheck, X } from "lucide-react";

import { cn } from "@/lib/utils";
import { Badge } from "@/components/ui/badge";
import { AdminPage, Panel } from "./admin-page";
import { MockBanner } from "./mock-banner";

type Log = {
  ts: string;
  allow: boolean;
  actor: string;
  action: string;
  resource: string;
  reason: string;
};

/// サンプルの監査レコード（API は Task 8.1 / 8.2 で実装）。
const LOGS: Log[] = [
  { ts: "10:21:44.031", allow: true, actor: "佐藤 美咲", action: "rag.cite", resource: "chunk:f_91a2#14", reason: "閲覧者（営業1課 経由）" },
  { ts: "10:21:07.412", allow: false, actor: "佐藤 美咲", action: "file.edit", resource: "file:f_91a2", reason: "閲覧者のため編集不可" },
  { ts: "10:20:58.105", allow: true, actor: "小林 大輔", action: "folder.share", resource: "folder:d_33c0", reason: "所有者" },
  { ts: "10:20:12.009", allow: false, actor: "佐藤 美咲", action: "secret.use", resource: "secret:s_04bd", reason: "利用権限なし" },
  { ts: "10:19:55.742", allow: true, actor: "週次売上レポート", action: "data.read", resource: "data_table:dt_51", reason: "委譲（山本 理恵）" },
];

/// 監査ログ（モック）。行を選ぶと右に詳細を出す。
export function MockAuditView() {
  const [selected, setSelected] = React.useState(1);
  const log = LOGS[selected];
  return (
    <AdminPage
      title="監査ログ"
      description={
        <span className="inline-flex items-center gap-1.5">
          <ShieldCheck className="size-3.5" aria-hidden />
          操作・権限判定・引用チャンクを改ざん検知付きで記録します。
        </span>
      }
      wide
    >
      <MockBanner what="監査ダッシュボードの API は Task 8.1 / 8.2 で実装予定" />
      <div className="grid gap-4 lg:grid-cols-[minmax(0,1fr)_340px]">
        <Panel>
          <div className="overflow-x-auto">
            <table className="w-full min-w-[640px] border-collapse text-[12.5px]">
              <thead>
                <tr className="bg-muted/40 text-left text-xs text-muted-foreground">
                  <th scope="col" className="px-4 py-2 font-medium">時刻</th>
                  <th scope="col" className="px-2 py-2 font-medium">判定</th>
                  <th scope="col" className="px-2 py-2 font-medium">誰が</th>
                  <th scope="col" className="px-2 py-2 font-medium">操作</th>
                  <th scope="col" className="px-4 py-2 font-medium">対象</th>
                </tr>
              </thead>
              <tbody className="font-mono">
                {LOGS.map((l, i) => (
                  <tr
                    key={l.ts}
                    className={cn("border-t", i === selected ? "bg-accent/60" : "hover:bg-accent/30")}
                  >
                    <td className="px-4 py-2 text-muted-foreground">
                      <button
                        type="button"
                        onClick={() => setSelected(i)}
                        className="outline-none hover:underline focus-visible:ring-2 focus-visible:ring-ring"
                      >
                        {l.ts}
                      </button>
                    </td>
                    <td className="px-2 py-2 font-sans">
                      <Badge variant={l.allow ? "success" : "destructive"} className="gap-1">
                        {l.allow ? <Check className="size-3" aria-hidden /> : <X className="size-3" aria-hidden />}
                        {l.allow ? "許可" : "拒否"}
                      </Badge>
                    </td>
                    <td className="px-2 py-2 font-sans">{l.actor}</td>
                    <td className="px-2 py-2">{l.action}</td>
                    <td className="px-4 py-2 text-muted-foreground">{l.resource}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </Panel>
        <Panel title={log.action}>
          <dl className="grid grid-cols-[72px_minmax(0,1fr)] gap-x-3 gap-y-2 px-4 py-3 text-[12.5px]">
            <dt className="text-muted-foreground">時刻</dt>
            <dd className="font-mono">2026-10-09 {log.ts}</dd>
            <dt className="text-muted-foreground">誰が</dt>
            <dd>{log.actor}</dd>
            <dt className="text-muted-foreground">対象</dt>
            <dd className="font-mono">{log.resource}</dd>
            <dt className="text-muted-foreground">理由</dt>
            <dd>{log.reason}</dd>
          </dl>
        </Panel>
      </div>
    </AdminPage>
  );
}
