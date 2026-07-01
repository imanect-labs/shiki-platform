"use client";

import * as React from "react";
import {
  ArrowDown,
  ArrowUp,
  ChevronDown,
  ChevronRight,
  File as FileIcon,
  FileArchive,
  FileAudio,
  FileCode,
  FileImage,
  FileSpreadsheet,
  FileText,
  FileVideo,
  Folder,
  Home,
  Loader2,
  Presentation,
} from "lucide-react";

import type { CrumbResponse, SortField } from "@/lib/storage";
import { cn } from "@/lib/utils";

/// 一覧の列レイアウト（ヘッダーと各行で共有）。OneDrive 風に
/// 名前（可変）｜更新日時｜更新者｜サイズ｜共有｜操作。
/// 画面幅で段階的に列を出す: 〜sm=名前+操作 / sm=更新日時+サイズ追加 / lg=更新者+共有も。
/// セルの DOM 順は常に固定し、隠す列は hidden で外す（grid 自動配置が崩れない）。
export const LIST_GRID =
  "grid items-center gap-3 grid-cols-[minmax(0,1fr)_40px] " +
  "sm:grid-cols-[minmax(0,1fr)_9.5rem_5.5rem_40px] " +
  "lg:grid-cols-[minmax(0,1fr)_9.5rem_8rem_5.5rem_6rem_40px]";

/// 拡張子/Content-Type からファイル種別アイコンと色を決める（OneDrive 風）。
/// 種別色はファイルアイコンの慣習色（PDF=赤・Word=青・Excel=緑・PowerPoint=橙…）に合わせる。
function fileIcon(name: string, contentType?: string | null): { Icon: typeof FileIcon; color: string } {
  const ext = name.toLowerCase().split(".").pop() ?? "";
  const ct = (contentType ?? "").toLowerCase();
  const is = (...exts: string[]) => exts.includes(ext);

  if (ext === "pdf" || ct === "application/pdf") return { Icon: FileText, color: "text-red-500" };
  if (is("doc", "docx", "rtf") || ct.includes("word")) return { Icon: FileText, color: "text-blue-600" };
  if (is("xls", "xlsx", "csv") || ct.includes("sheet") || ct.includes("excel"))
    return { Icon: FileSpreadsheet, color: "text-green-600" };
  if (is("ppt", "pptx") || ct.includes("presentation") || ct.includes("powerpoint"))
    return { Icon: Presentation, color: "text-orange-500" };
  if (is("png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "ico") || ct.startsWith("image/"))
    return { Icon: FileImage, color: "text-purple-500" };
  if (is("zip", "tar", "gz", "rar", "7z") || ct.includes("zip"))
    return { Icon: FileArchive, color: "text-amber-600" };
  if (is("json", "js", "ts", "tsx", "jsx", "py", "rs", "go", "java", "html", "css", "yml", "yaml", "toml", "sh"))
    return { Icon: FileCode, color: "text-sky-600" };
  if (is("mp3", "wav", "flac", "m4a", "aac") || ct.startsWith("audio/"))
    return { Icon: FileAudio, color: "text-pink-500" };
  if (is("mp4", "mov", "avi", "mkv", "webm") || ct.startsWith("video/"))
    return { Icon: FileVideo, color: "text-rose-500" };
  if (is("md", "markdown", "txt", "log")) return { Icon: FileText, color: "text-foreground/60" };
  return { Icon: FileIcon, color: "text-muted-foreground" };
}

/// ノード種別アイコン。フォルダは OneDrive 風の黄色、ファイルは種別アイコン＋慣習色。
export function NodeIcon({
  kind,
  name = "",
  contentType,
  className,
}: {
  kind: string;
  name?: string;
  contentType?: string | null;
  className?: string;
}) {
  if (kind === "folder") {
    // 塗りつぶしの黄色フォルダ（OneDrive 風）。
    return <Folder className={cn("size-6 fill-amber-400 text-amber-500", className)} aria-hidden />;
  }
  const { Icon, color } = fileIcon(name, contentType);
  return <Icon className={cn("size-6", color, className)} aria-hidden />;
}

/// パンくず（root→自身）。各要素クリックでそのフォルダへ遷移する。
export function Breadcrumbs({
  crumbs,
  onNavigate,
}: {
  crumbs: CrumbResponse[];
  /// `null` でルートへ。
  onNavigate: (id: string | null) => void;
}) {
  return (
    <nav aria-label="パンくず" className="flex min-w-0 items-center gap-1 text-sm">
      <button
        type="button"
        onClick={() => onNavigate(null)}
        className="flex shrink-0 items-center gap-1 rounded-md px-1.5 py-1 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
      >
        <Home className="size-4" aria-hidden />
        <span>ドライブ</span>
      </button>
      {crumbs.map((c, i) => {
        const last = i === crumbs.length - 1;
        return (
          <React.Fragment key={c.id}>
            <ChevronRight className="size-4 shrink-0 text-muted-foreground/60" aria-hidden />
            <button
              type="button"
              onClick={() => onNavigate(c.id)}
              aria-current={last ? "page" : undefined}
              className={cn(
                "truncate rounded-md px-1.5 py-1 transition-colors hover:bg-accent",
                last ? "font-medium text-foreground" : "text-muted-foreground hover:text-foreground",
              )}
            >
              {c.name}
            </button>
          </React.Fragment>
        );
      })}
    </nav>
  );
}

/// クリックでソートする列見出し（OneDrive 風）。非アクティブ列はホバーで ⌄ を見せ、
/// アクティブ列は太字＋方向矢印（↑/↓）で現在の並び順を示す。
function SortLabel({
  label,
  field,
  sort,
  desc,
  onSort,
  className,
}: {
  label: string;
  field: SortField;
  sort: SortField;
  desc: boolean;
  onSort: (field: SortField) => void;
  className?: string;
}) {
  const active = sort === field;
  return (
    <button
      type="button"
      onClick={() => onSort(field)}
      className={cn(
        "group/sort flex items-center gap-1 rounded-md py-0.5 transition-colors hover:text-foreground",
        active ? "text-foreground" : "text-muted-foreground",
        className,
      )}
    >
      <span className={cn("truncate", active && "font-semibold")}>{label}</span>
      {active ? (
        desc ? (
          <ArrowDown className="size-3.5 shrink-0" aria-hidden />
        ) : (
          <ArrowUp className="size-3.5 shrink-0" aria-hidden />
        )
      ) : (
        <ChevronDown
          className="size-3 shrink-0 opacity-0 transition-opacity group-hover/sort:opacity-50"
          aria-hidden
        />
      )}
    </button>
  );
}

/// 一覧の列見出し行（OneDrive 風）。名前/更新日時/サイズはクリックでソート、
/// 更新者/共有は表示のみ（サーバ側ソート未対応）。列の出し入れは LIST_GRID と揃える。
export function ListHeader({
  sort,
  desc,
  onSort,
}: {
  sort: SortField;
  desc: boolean;
  onSort: (field: SortField) => void;
}) {
  return (
    <div
      className={cn(
        LIST_GRID,
        "shiki-dash-bottom px-3 pb-2 text-[13px] font-medium text-muted-foreground",
      )}
    >
      <SortLabel label="名前" field="name" sort={sort} desc={desc} onSort={onSort} />
      <SortLabel
        label="更新日時"
        field="updated"
        sort={sort}
        desc={desc}
        onSort={onSort}
        className="hidden sm:flex"
      />
      <span className="hidden truncate lg:block">更新者</span>
      <SortLabel
        label="サイズ"
        field="size"
        sort={sort}
        desc={desc}
        onSort={onSort}
        className="hidden sm:flex"
      />
      <span className="hidden truncate lg:block">共有</span>
      <span aria-hidden />
    </div>
  );
}

/// 一覧フッタのローディング行（無限スクロールの sentinel と併用）。
export function LoadingRow({ label = "読み込み中…" }: { label?: string }) {
  return (
    <div className="flex items-center justify-center gap-2 py-4 text-sm text-muted-foreground">
      <Loader2 className="size-4 animate-spin" aria-hidden />
      <span>{label}</span>
    </div>
  );
}
