"use client";

import * as React from "react";

import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { CopyLinkButton } from "@/components/share/copy-link-button";
import { ShareLinksPanel } from "@/components/share/share-links-panel";
import { SharePeoplePanel } from "@/components/share/share-people-panel";
import { SHARE_DIALOG_CONTENT, ShareDialogFooter } from "@/components/share/share-dialog-layout";
import {
  listShares,
  shareNode,
  unshareNode,
  type NodeResponse,
  type ShareTarget,
} from "@/lib/storage";
import { resourcePath, supportsPasswordUnlock } from "@/lib/resource-link";

/// 共有対象の最小形。NodeResponse は構造的に適合するため既存のドライブ呼び出しは無変更で、
/// ノート/Office エディタは `{ id, name }` を直接渡せる（kind/content_type はリンク解決の任意情報）。
export type ShareTargetNode = Pick<NodeResponse, "id" | "name"> &
  Partial<Pick<NodeResponse, "kind" | "content_type">>;

/// 共有ダイアログ（#342）。2 タブ構成（スマホは下からのシート・Dialog が自動で切り替える）:
/// - **メンバー・部署**: 特定のメンバー/部署・ロールへ直接共有（ReBAC 付与）。1 つの検索欄でまとめて探し、
///   付ける権限は検索欄の横で選ぶ（共有後は相手ごとに変更・解除できる）。タブ名の横に件数を出す。
/// - **リンク**: 共有リンクの発行（範囲/権限/期限/パスワード）＋発行済み一覧（コピー/延長/失効）。
/// 下端には既存の権限者へ渡すリンクのコピーを固定する（付与ゼロの純ポインタ）。
/// 別テナントの相手は検索結果に出ない（サーバ側 tenant_id スコープ）。部署・ロールはそのメンバー
/// （配下ロール込み）へ一括共有される（#76）。
export function ShareDialog({
  open,
  onOpenChange,
  node,
  shareUrl,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  node: ShareTargetNode | null;
  /// 共有リンクのパス（例 `/notes/{id}`）。エディタから開くとき name から拡張子が落ちて種別を
  /// 判定できないケースのために明示指定する。省略時は node から解決する。
  shareUrl?: string;
}) {
  // タブに出す件数（読み込み中・権限なしは null で出さない）。
  const [peopleCount, setPeopleCount] = React.useState<number | null>(null);
  const [linkCount, setLinkCount] = React.useState<number | null>(null);
  const contentRef = React.useRef<HTMLDivElement>(null);

  if (!node) return null;

  const linkPath =
    shareUrl ??
    resourcePath({ id: node.id, name: node.name, kind: node.kind, content_type: node.content_type });
  const pointerUrl = () => {
    const origin = typeof window !== "undefined" ? window.location.origin : "";
    return `${origin}${linkPath}`;
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        className={SHARE_DIALOG_CONTENT}
        ref={contentRef}
        // 開いた直後のフォーカス: マウス・トラックパッドでは検索欄へ（すぐ相手を探せる）。タッチでは
        // キーボードが立ち上がって画面が隠れるので、ダイアログ本体に置く。既定（先頭のタブ）だと、
        // タブに太いフォーカスリングが出て「選択中の枠」に見え紛らわしかった。
        onOpenAutoFocus={(e) => {
          e.preventDefault();
          const root = contentRef.current;
          const search = root?.querySelector<HTMLInputElement>("[data-share-search]");
          if (search && window.matchMedia("(pointer: fine)").matches) search.focus();
          else root?.focus();
        }}
      >
        <DialogHeader>
          <DialogTitle className="leading-snug [overflow-wrap:anywhere]">「{node.name}」を共有</DialogTitle>
          <DialogDescription>
            メンバーや部署に権限を付けるか、共有リンクを発行します。
          </DialogDescription>
        </DialogHeader>

        {/* min-w-0: DialogContent は grid のため、子が折り返さない文字（メール等）の幅まで広がって
            横にはみ出すのを防ぐ。 */}
        <Tabs defaultValue="permissions" className="mt-4 min-w-0">
          <TabsList className="grid w-full grid-cols-2 pointer-coarse:h-12">
            <TabsTrigger value="permissions" data-testid="share-tab-permissions">
              メンバー・部署
              <TabCount count={peopleCount} />
            </TabsTrigger>
            <TabsTrigger value="links" data-testid="share-tab-links">
              リンク
              <TabCount count={linkCount} />
            </TabsTrigger>
          </TabsList>

          {/* 両タブとも開いた時点で読み込む（forceMount）。タブ名の件数を、開く前から出すため。 */}
          <TabsContent value="permissions" forceMount className="mt-3 data-[state=inactive]:hidden">
            <SharePeoplePanel
              open={open}
              resourceId={node.id}
              load={() => listShares(node.id)}
              grant={(target, role) => shareNode(node.id, target as ShareTarget, role)}
              revoke={(target, role) => unshareNode(node.id, target as ShareTarget, role)}
              onCountChange={setPeopleCount}
            />
          </TabsContent>

          <TabsContent value="links" forceMount className="mt-3 data-[state=inactive]:hidden">
            <ShareLinksPanel
              nodeId={node.id}
              linkPath={linkPath}
              // パスワード付きリンクは解錠画面のあるページ（ノート/Office/スライド/CSV）でしか開けない。
              // フォルダ・PDF・ビューアの無いファイルは解錠画面が無いため、パスワード発行を無効化する（C-1）。
              passwordSupported={supportsPasswordUnlock(linkPath)}
              onCountChange={setLinkCount}
            />
          </TabsContent>
        </Tabs>

        <ShareDialogFooter>
          <span className="min-w-0 text-xs text-muted-foreground">アクセス権のある人へリンクを渡す</span>
          <CopyLinkButton url={pointerUrl} className="shrink-0" />
        </ShareDialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/// タブ名の横の件数（0 件・未取得は出さない）。
function TabCount({ count }: { count: number | null }) {
  if (!count) return null;
  return (
    <span className="ml-1.5 rounded-full bg-muted-foreground/15 px-1.5 text-[11px] font-medium leading-4 tabular-nums">
      {count}
    </span>
  );
}
