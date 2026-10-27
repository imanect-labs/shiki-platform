"use client";

import * as React from "react";
import Link from "next/link";
import { ChevronDown, HardDrive } from "lucide-react";

import { cn } from "@/lib/utils";
import type { ArtifactMeta } from "@/lib/artifact-api";
import type { AutonomousMode, WorkspaceChoice } from "@/lib/chat-api";
import { CREATE_KINDS, type CreateKind, type HomeMode } from "@/lib/home-modes";
import { APPROVAL_MODES, approvalModeDef } from "@/components/chat/approval-mode-select";
import { FolderPicker } from "@/components/artifacts/folder-picker";
import { SkillPicker } from "@/components/home/skill-picker";
import { HOME_MODE_PANEL_ID, homeModeTabId } from "./home-mode-tabs";
import { useRovingSelect } from "./use-roving-select";

const APPROVAL_KEYS = APPROVAL_MODES.map((m) => m.mode);
const CREATE_KEYS = CREATE_KINDS.map((k) => k.key);

/// モード切替の下に 1 段だけ出す、そのモードの設定（#964）。
/// - チャット: スキル（無ければ一言の説明）
/// - 社内検索: 何をするかの説明と、検索画面への導線
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
}) {
  const [wsPickerOpen, setWsPickerOpen] = React.useState(false);
  const approvalRoving = useRovingSelect(APPROVAL_KEYS, approvalMode, onApprovalModeChange);
  const createRoving = useRovingSelect(CREATE_KEYS, createKind, onCreateKindChange);
  const approval = approvalModeDef(approvalMode);

  return (
    <div
      id={HOME_MODE_PANEL_ID}
      role="tabpanel"
      aria-labelledby={homeModeTabId(mode)}
      className="flex min-h-8 w-full justify-center"
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
        <Hint>見る権限のある社内文書だけから探して、出典つきで答えます</Hint>
        <Link
          href="/search"
          className="rounded text-xs text-foreground/70 underline-offset-4 outline-none hover:text-foreground hover:underline focus-visible:ring-2 focus-visible:ring-ring"
        >
          検索画面で探す
        </Link>
      </Row>

      <div hidden={mode !== "agent"} className="flex flex-col items-center gap-1.5">
        <Row>
          <span className="inline-flex items-center gap-2">
            <span className="text-xs text-muted-foreground">作業フォルダ</span>
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
            <span id="home-approval-label" className="text-xs text-muted-foreground">
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

      <Row hidden={mode !== "create"} role="radiogroup" aria-label="作るもの">
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
    </div>
  );
}

/// 段の中の丸いチップ（作業フォルダ・作るもの）。
const OPTION =
  "inline-flex h-8 items-center gap-1.5 whitespace-nowrap rounded-full border border-border bg-card px-3 text-[12.5px] outline-none transition-colors hover:border-foreground/20 hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring";

function Row({
  hidden = false,
  children,
  ...aria
}: {
  hidden?: boolean;
  children: React.ReactNode;
  role?: string;
  "aria-label"?: string;
}) {
  return (
    <div
      hidden={hidden}
      {...aria}
      className="flex flex-wrap items-center justify-center gap-x-2 gap-y-1.5"
    >
      {children}
    </div>
  );
}

function Hint({ children }: { children: React.ReactNode }) {
  return <span className="text-xs text-muted-foreground">{children}</span>;
}
