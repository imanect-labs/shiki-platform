"use client";

/// ファイル・フォルダの移動（`drive.move`）の承認カードの中身（#942）: 生の id と JSON の代わりに、
/// 何を（今の場所）・どこへ・どんな名前で移すかを読める形で見せる。
///
/// 名前・場所は承認する人本人の権限で読む（読めないものは id のまま）。表示のための読み取りで、
/// 実行の権限は StorageService が別に検査する。

import { ArrowRight, Folder, Loader2 } from "lucide-react";
import * as React from "react";

import { breadcrumb, type CrumbResponse } from "@/lib/storage";

type MoveInput = {
  node_id?: unknown;
  destination_folder_id?: unknown;
  new_name?: unknown;
};

const ROOT_LABEL = "マイドライブ（ルート）";

function pathOf(crumbs: CrumbResponse[]): string {
  return crumbs.map((c) => c.name).join(" / ");
}

export function DriveMoveSummary({ input }: { input: unknown }) {
  const p = (input ?? {}) as MoveInput;
  const nodeId = typeof p.node_id === "string" ? p.node_id : null;
  const dest = typeof p.destination_folder_id === "string" ? p.destination_folder_id.trim() : "";
  const newName = typeof p.new_name === "string" && p.new_name.trim() ? p.new_name.trim() : null;
  const [state, setState] = React.useState<{
    loading: boolean;
    target: CrumbResponse[] | null;
    destination: CrumbResponse[] | null;
  }>({ loading: true, target: null, destination: null });

  React.useEffect(() => {
    let active = true;
    void (async () => {
      const [target, destination] = await Promise.all([
        nodeId ? breadcrumb(nodeId).catch(() => null) : Promise.resolve(null),
        dest && dest !== "root" ? breadcrumb(dest).catch(() => null) : Promise.resolve(null),
      ]);
      if (active) setState({ loading: false, target, destination });
    })();
    return () => {
      active = false;
    };
  }, [nodeId, dest]);

  if (state.loading) {
    return (
      <p className="mb-2 flex items-center gap-1.5 text-[13px] text-muted-foreground">
        <Loader2 className="size-3.5 animate-spin" aria-hidden />
        移動の内容を確認しています…
      </p>
    );
  }

  const self = state.target?.[state.target.length - 1];
  const name = self?.name ?? nodeId ?? "（不明）";
  // 今の場所は自身を除いた祖先。読める祖先が無ければ「マイドライブ」直下とは限らないので出さない。
  const from = state.target && state.target.length > 1 ? pathOf(state.target.slice(0, -1)) : null;
  const to = !dest ? null : dest === "root" ? ROOT_LABEL : state.destination ? pathOf(state.destination) : dest;

  return (
    <div className="mb-2 space-y-1.5 text-[13px]" data-testid="drive-move-summary">
      <p className="text-foreground/80 [overflow-wrap:anywhere]">
        {self?.kind === "folder" ? "フォルダ" : "ファイル"}「
        <span className="font-medium text-foreground">{name}</span>」を
        {to ? "移動します。" : "名前変更します。"}
      </p>
      {to ? (
        <p className="flex flex-wrap items-center gap-1.5 text-[12px] text-muted-foreground">
          <Folder className="size-3.5 shrink-0" aria-hidden />
          <span className="[overflow-wrap:anywhere]">{from ?? "今の場所"}</span>
          <ArrowRight className="size-3.5 shrink-0" aria-hidden />
          <span className="font-medium text-foreground [overflow-wrap:anywhere]">{to}</span>
        </p>
      ) : null}
      {newName ? (
        <p className="text-[12px] text-muted-foreground [overflow-wrap:anywhere]">
          新しい名前: <span className="font-medium text-foreground">{newName}</span>
        </p>
      ) : null}
    </div>
  );
}
