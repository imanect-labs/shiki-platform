"use client";

import * as React from "react";
import Link from "next/link";
import { ChevronDown, FolderSearch, HardDrive, X } from "lucide-react";

import { cn } from "@/lib/utils";
import type { ArtifactMeta } from "@/lib/artifact-api";
import type { AutonomousMode, WorkspaceChoice } from "@/lib/chat-api";
import { CREATE_KINDS, type CreateKind, type HomeMode } from "@/lib/home-modes";
import { APPROVAL_MODES, approvalModeDef } from "@/components/chat/approval-mode-select";
import { FolderPicker } from "@/components/artifacts/folder-picker";
import { SkillPicker } from "@/components/home/skill-picker";
import { HOME_MODE_PANEL_ID, homeModeTabId } from "./home-mode-tabs";
import { useRovingSelect } from "./use-roving-select";

/// 社内検索で探すフォルダ（ホームで選んだもの）。
export type SearchFolder = { id: string; name: string };

const APPROVAL_KEYS = APPROVAL_MODES.map((m) => m.mode);
const CREATE_KEYS = CREATE_KINDS.map((k) => k.key);

/// モード切替の下に 1 段だけ出す、そのモードの設定（#964）。
/// - チャット: スキル（無ければ一言の説明）
/// - 社内検索: 探すフォルダ（未指定は見られる社内文書すべて）と、検索画面への導線
/// - エージェント: 作業フォルダ（ワークスペース）と承認モード
/// - 作成: 作るものの種類
///
/// どのモードの段も全部描いておき、選んでいないものは隠す（切り替えるたびにスキル一覧を読み直さない）。
export function HomeModeOptions({
  mode,
  skill,
  onSkillChange,
  workspace,
  onWorkspaceChange,
  approvalMode,
  onApprovalModeChange,
  createKind,
  onCreateKindChange,
  searchFolder,
  onSearchFolderChange,
}: {
  mode: HomeMode;
  skill: ArtifactMeta | null;
  onSkillChange: (skill: ArtifactMeta | null) => void;
  workspace: WorkspaceChoice | null;
  onWorkspaceChange: (w: WorkspaceChoice | null) => void;
  approvalMode: AutonomousMode;
  onApprovalModeChange: (m: AutonomousMode) => void;
  createKind: CreateKind;
  onCreateKindChange: (k: CreateKind) => void;
  /// 社内検索で探すフォルダ（null は見られる社内文書すべて・#965）。
  searchFolder: SearchFolder | null;
  onSearchFolderChange: (f: SearchFolder | null) => void;
}) {
  const [wsPickerOpen, setWsPickerOpen] = React.useState(false);
  const [searchPickerOpen, setSearchPickerOpen] = React.useState(false);
  const approvalRoving = useRovingSelect(APPROVAL_KEYS, approvalMode, onApprovalModeChange);
  const createRoving = useRovingSelect(CREATE_KEYS, createKind, onCreateKindChange);
  const approval = approvalModeDef(approvalMode);

  return (
    <div
      id={HOME_MODE_PANEL_ID}
      role="tabpanel"
      aria-labelledby={homeModeTabId(mode)}
      // スマホでは、チャット・社内検索の段（スキル・説明）は出さない（スキルは「+」から選べる）。
      className={cn(
        "flex w-full justify-center sm:min-h-8",
        mode === "chat" && "max-sm:hidden",
      )}
      data-testid="home-mode-options"
    >
      <Row hidden={mode !== "chat"}>
        <SkillPicker
          selected={skill}
          onSelect={onSkillChange}
          empty={<Hint>必要に応じて社内文書や Web も調べて答えます</Hint>}
        />
      </Row>

      <Row hidden={mode !== "search"}>
        {/* エージェントの作業フォルダと同じ形。選ぶとその会話の社内文書検索をフォルダの配下に絞る。 */}
        <span className="inline-flex items-center gap-2">
          <span className="sr-only text-xs text-muted-foreground sm:not-sr-only">探すフォルダ</span>
          <span className="inline-flex items-center">
            <button
              type="button"
              onClick={() => setSearchPickerOpen(true)}
              aria-label={`探すフォルダ: ${searchFolder ? searchFolder.name : "すべての社内文書"}`}
              title="社内文書を探すフォルダを選ぶ"
              data-testid="home-search-folder"
              className={cn(
                OPTION,
                searchFolder
                  ? "rounded-r-none border-r-0 border-[var(--season-winter)]/50 bg-[var(--season-winter)]/[0.08] text-foreground"
                  : "text-foreground/75",
              )}
            >
              <FolderSearch className="size-3.5 shrink-0" aria-hidden />
              <span className="max-w-[180px] truncate">
                {searchFolder ? searchFolder.name : "すべての社内文書"}
              </span>
              {searchFolder ? null : (
                <ChevronDown className="size-3.5 shrink-0 text-muted-foreground" aria-hidden />
              )}
            </button>
            {searchFolder ? (
              <button
                type="button"
                onClick={() => onSearchFolderChange(null)}
                aria-label="フォルダの指定を外す"
                className="inline-flex h-8 items-center rounded-r-full border border-l-0 border-[var(--season-winter)]/50 bg-[var(--season-winter)]/[0.08] pl-1 pr-2.5 text-muted-foreground outline-none transition-colors hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
              >
                <X className="size-3.5" aria-hidden />
              </button>
            ) : null}
          </span>
        </span>
        <Link
          href="/search"
          className="hidden rounded text-xs text-foreground/70 underline-offset-4 outline-none hover:text-foreground hover:underline focus-visible:ring-2 focus-visible:ring-ring sm:inline"
        >
          検索画面で探す
        </Link>
      </Row>

      <div hidden={mode !== "agent"} className="flex flex-col items-center gap-1.5">
        <Row>
          <span className="inline-flex items-center gap-2">
            {/* スマホは見出しの語を省いて 1 行に収める（読み上げには残す）。 */}
            <span className="sr-only text-xs text-muted-foreground sm:not-sr-only">作業フォルダ</span>
            <button
              type="button"
              onClick={() => setWsPickerOpen(true)}
              aria-label={`作業フォルダ: ${workspace ? workspace.folderName : "マイドライブ"}`}
              title="エージェントが作業するワークスペースの場所を選ぶ"
              className={cn(OPTION, "text-foreground/75")}
            >
              <HardDrive className="size-3.5 shrink-0" aria-hidden />
              <span className="max-w-[160px] truncate">
                {workspace ? workspace.folderName : "マイドライブ"}
              </span>
              <ChevronDown className="size-3.5 shrink-0 text-muted-foreground" aria-hidden />
            </button>
          </span>
          <span className="inline-flex items-center gap-2 sm:ml-2">
            <span
              id="home-approval-label"
              className="sr-only text-xs text-muted-foreground sm:not-sr-only"
            >
              承認
            </span>
            <div
              role="radiogroup"
              aria-labelledby="home-approval-label"
              aria-describedby="home-approval-description"
              className="inline-flex gap-0.5 rounded-lg border border-border bg-muted/40 p-0.5"
            >
              {APPROVAL_MODES.map((m, i) => {
                const active = m.mode === approvalMode;
                return (
                  <button
                    key={m.mode}
                    ref={approvalRoving.ref(i)}
                    type="button"
                    role="radio"
                    aria-checked={active}
                    tabIndex={approvalRoving.tabIndex(i)}
                    title={m.description}
                    data-testid={`home-approval-${m.mode}`}
                    onClick={() => onApprovalModeChange(m.mode)}
                    onKeyDown={approvalRoving.onKeyDown(i)}
                    className={cn(
                      "h-6 rounded-md px-2.5 text-xs outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring",
                      active
                        ? "border border-border bg-background shadow-xs"
                        : "text-muted-foreground hover:text-foreground",
                      active && (m.danger ? "text-destructive" : "text-foreground"),
                    )}
                  >
                    {m.label}
                  </button>
                );
              })}
            </div>
          </span>
        </Row>
        {/* 既定（承認必須）から緩めたときは、何が起きるかを常に文字で出す（ホバーの無いタッチ端末でも分かる）。 */}
        <p
          id="home-approval-description"
          hidden={approvalMode === "require_approval"}
          className={cn(
            "text-center text-xs",
            approval.danger ? "text-destructive" : "text-muted-foreground",
          )}
        >
          {approval.description}
        </p>
      </div>

      <Row
        hidden={mode !== "create"}
        role="radiogroup"
        aria-label="作るもの"
        // スマホは折り返さずに横へ流す（2 段に積まない）。
        className="max-sm:w-full max-sm:flex-nowrap max-sm:justify-start max-sm:overflow-x-auto max-sm:px-1 max-sm:[scrollbar-width:none] max-sm:[mask-image:linear-gradient(to_right,black_85%,transparent)]"
      >
        {CREATE_KINDS.map((k, i) => {
          const active = k.key === createKind;
          const Icon = k.icon;
          return (
            <button
              key={k.key}
              ref={createRoving.ref(i)}
              type="button"
              role="radio"
              aria-checked={active}
              tabIndex={createRoving.tabIndex(i)}
              data-testid={`home-create-${k.key}`}
              onClick={() => onCreateKindChange(k.key)}
              onKeyDown={createRoving.onKeyDown(i)}
              className={cn(
                OPTION,
                active
                  ? "border-[var(--season-autumn)]/60 bg-[var(--season-autumn)]/[0.08] text-foreground"
                  : "text-foreground/70",
              )}
            >
              <Icon className="size-3.5 shrink-0" aria-hidden />
              {k.label}
            </button>
          );
        })}
      </Row>

      <FolderPicker
        open={wsPickerOpen}
        onOpenChange={setWsPickerOpen}
        purpose="workspace"
        onSelect={(f) => onWorkspaceChange({ mode: f.mode, folderId: f.id, folderName: f.name })}
      />
      <FolderPicker
        open={searchPickerOpen}
        onOpenChange={setSearchPickerOpen}
        purpose="search"
        onSelect={(f) => onSearchFolderChange({ id: f.id, name: f.name })}
      />
    </div>
  );
}

/// 段の中の丸いチップ（作業フォルダ・作るもの）。
const OPTION =
  "inline-flex h-8 items-center gap-1.5 whitespace-nowrap rounded-full border border-border bg-card px-3 text-[12.5px] outline-none transition-colors hover:border-foreground/20 hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring";

function Row({
  hidden = false,
  className,
  children,
  ...aria
}: {
  hidden?: boolean;
  className?: string;
  children: React.ReactNode;
  role?: string;
  "aria-label"?: string;
}) {
  return (
    <div
      hidden={hidden}
      {...aria}
      className={cn("flex flex-wrap items-center justify-center gap-x-2 gap-y-1.5", className)}
    >
      {children}
    </div>
  );
}

function Hint({ children }: { children: React.ReactNode }) {
  return <span className="text-xs text-muted-foreground">{children}</span>;
}
