"use client";

/// コンポーザ左の「+」メニュー。並びは PC・スマホとも 添付 → 作成 → ツール → スキル。
/// モデルは送信ボタンの左のボタンで選ぶ（#768）。
///
/// - PC（md 以上）: Radix DropdownMenu（ポータル描画＋衝突回避。画面中央のホームでも上端に
///   食い込まず、収まらない高さは内部スクロール）。作成はサブメニュー。
/// - スマホ: 下からのシート（ui/dialog はスマホで自動的に下からのシートになる）に、セクションを
///   縦に並べる。サブメニューは指で辿りにくいので、作成は横スクロールのチップにする。
///
/// セクションは `PlusSection` で区切った独立の部品にしてある。新しいセクション（例: 使うツールを
/// 選ぶ）は、選択状態を Composer の state に持たせ、props で渡してここへ 1 ブロック足せばよい。
///
/// 別の面（ドライブの選択・スキル一覧）を開く項目は、メニュー/シートが閉じきってから
/// 開く。閉じる途中で開くと、閉じる側の後始末（フォーカスを「+」へ戻す）が新しい面の外側操作と
/// 見なされて即座に閉じてしまう。

import * as React from "react";
import {
  Check,
  ChevronRight,
  Cpu,
  FilePlus2,
  FileSpreadsheet,
  FileText,
  HardDrive,
  ImagePlus,
  ListChecks,
  NotebookPen,
  Plus,
  Presentation,
  Sparkles,
  Upload,
} from "lucide-react";

import { cn } from "@/lib/utils";
import type { SlashSuggestion } from "@/lib/slash-command";
import { useMediaQuery } from "@/hooks/use-media-query";
import { useCreateContent } from "@/hooks/use-create-content";
import { Dialog, DialogContent, DialogDescription, DialogTitle } from "@/components/ui/dialog";
import {
  DropdownMenu,
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { SELECTABLE_TOOLS, useToolChoice } from "@/lib/tool-choice";
import { useModelSummary } from "./model-picker";
import {
  SkillBrowser,
  SkillRow,
  SkillSearchInput,
  filterSkills,
  recordRecentSkill,
  useRankedSkills,
} from "./skill-browser";

/// PC のメニューに並べるスキルの件数（残りは「すべてのスキル」の一覧から）。
const MAX_MENU_SKILLS = 5;
/// スマホのシートに並べるスキルの件数（検索中は少し多めに）。
const SHEET_SKILLS = 3;
const SHEET_SKILLS_SEARCH = 6;

export type PlusMenuProps = {
  open: boolean;
  onOpenChange: (v: boolean) => void;
  onUploadLocal: () => void;
  onOpenDrive: () => void;
  /// モデル選択のスコープ（スレッド id かホーム・#553）。
  modelScope: string;
  /// 「作成」の保存先フォルダ（エージェントモードのワークスペース選択時はそれ・既定はマイドライブ直下）。
  createParentId?: string | null;
  /// スラッシュコマンドを持つスキル（#387）。選ぶと入力欄へコマンドが確定する。
  skills?: SlashSuggestion[];
  onPickSkill?: (s: SlashSuggestion) => void;
  /// スマホのシートに「モデル」の行を出し、押したら選ぶ面を開く（入力欄にモデルを出していないとき）。
  onOpenModel?: () => void;
};

/// 閉じきってから走らせる操作の予約（ドライブ・スキル一覧を開く）。
function useAfterClose() {
  const pending = React.useRef<(() => void) | null>(null);
  const onCloseAutoFocus = React.useCallback((e: Event) => {
    const run = pending.current;
    if (!run) return;
    pending.current = null;
    e.preventDefault();
    run();
  }, []);
  return { pending, onCloseAutoFocus };
}

export function PlusMenu(props: PlusMenuProps) {
  const desktop = useMediaQuery("(min-width: 768px)");
  const [skillsOpen, setSkillsOpen] = React.useState(false);
  const { open, onOpenChange, skills = [], onPickSkill } = props;
  const pickSkill = React.useCallback(
    (s: SlashSuggestion) => {
      recordRecentSkill(s.key);
      onPickSkill?.(s);
    },
    [onPickSkill],
  );

  const button = (
    <button
      type="button"
      aria-label="追加メニューを開く"
      title="追加（添付・作成・ツール・スキル）"
      aria-haspopup={desktop ? "menu" : "dialog"}
      aria-expanded={open}
      onClick={desktop ? undefined : () => onOpenChange(!open)}
      className={cn(
        "flex size-10 shrink-0 items-center justify-center rounded-full transition-colors md:size-9 md:border",
        "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
        open
          ? "bg-secondary text-foreground md:border-foreground/30"
          : "text-foreground/70 hover:bg-secondary hover:text-foreground md:border-border",
      )}
    >
      <Plus className={cn("size-5 transition-transform md:size-[18px]", open && "rotate-45")} aria-hidden />
    </button>
  );

  const shared = { ...props, pickSkill, openSkills: () => setSkillsOpen(true) };

  return (
    <>
      {desktop ? (
        <DesktopPlusMenu {...shared} button={button} />
      ) : (
        <>
          {button}
          <PlusSheet {...shared} />
        </>
      )}
      {skills.length > 0 && onPickSkill ? (
        <SkillBrowser open={skillsOpen} onOpenChange={setSkillsOpen} skills={skills} onPick={pickSkill} />
      ) : null}
    </>
  );
}

type SharedProps = PlusMenuProps & {
  pickSkill: (s: SlashSuggestion) => void;
  openSkills: () => void;
};

// ── PC: ドロップダウン ─────────────────────────────────────────────

function DesktopPlusMenu({
  open,
  onOpenChange,
  onUploadLocal,
  onOpenDrive,
  createParentId = null,
  skills = [],
  onPickSkill,
  pickSkill,
  modelScope,
  openSkills,
  button,
}: SharedProps & {
  button: React.ReactElement;
}) {
  // 作成ロジックはドライブの「新規作成」と共通（use-create-content・重複実装しない）。
  const create = useCreateContent({ parentId: createParentId });
  const after = useAfterClose();
  const ranked = useRankedSkills(skills, open);
  return (
    <DropdownMenu open={open} onOpenChange={onOpenChange}>
      <DropdownMenuTrigger asChild>{button}</DropdownMenuTrigger>

      <DropdownMenuContent
        side="top"
        align="start"
        sideOffset={8}
        collisionPadding={12}
        className="max-h-[var(--radix-dropdown-menu-content-available-height)] w-72 overflow-y-auto p-1.5"
        onCloseAutoFocus={after.onCloseAutoFocus}
      >
        {/* 添付 */}
        <DropdownMenuLabel className="uppercase tracking-wide">添付</DropdownMenuLabel>
        <DropdownMenuItem className="gap-2.5 px-2.5 py-2" onSelect={onUploadLocal}>
          <Upload className="text-muted-foreground" />
          ローカルからアップロード
        </DropdownMenuItem>
        <DropdownMenuItem
          className="gap-2.5 px-2.5 py-2"
          onSelect={() => {
            after.pending.current = onOpenDrive;
          }}
        >
          <HardDrive className="text-muted-foreground" />
          ドライブから選択
        </DropdownMenuItem>

        <DropdownMenuSeparator />

        {/* 作成（ノート/ドキュメント/スライド/CSV・#333）。作成後は対応エディタへ遷移する。 */}
        <DropdownMenuSub>
          <DropdownMenuSubTrigger className="gap-2.5 px-2.5 py-2" data-testid="composer-create-menu">
            <FilePlus2 className="text-muted-foreground" />
            作成
          </DropdownMenuSubTrigger>
          <DropdownMenuSubContent className="w-56 p-1.5">
            {CREATE_ITEMS.map((item) => (
              <DropdownMenuItem
                key={item.key}
                className="gap-2.5 px-2.5 py-2"
                onSelect={() => void create[item.fn]()}
                data-testid={`composer-create-${item.key}`}
              >
                <item.icon className={item.iconClass} aria-hidden />
                {item.label}
              </DropdownMenuItem>
            ))}
          </DropdownMenuSubContent>
        </DropdownMenuSub>

        {/* ツール（#734）。選んだツールはこの発話にだけ優先して使われる。選んでもメニューは閉じない。 */}
        <DesktopToolItems scope={modelScope} />

        {/* スキル（#387・#390）。最近使ったもの → 先頭の数件。残りは「すべてのスキル」から。 */}
        {skills.length > 0 && onPickSkill ? (
          <>
            <DropdownMenuSeparator />
            <DropdownMenuLabel className="uppercase tracking-wide">スキル</DropdownMenuLabel>
            {ranked.slice(0, MAX_MENU_SKILLS).map((s) => (
              <DropdownMenuItem
                key={s.key}
                className="gap-2.5 px-2.5 py-2"
                data-testid="composer-skill-command"
                onSelect={() => pickSkill(s)}
              >
                <Sparkles className="text-primary" aria-hidden />
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-sm">/{s.token}</span>
                  <span className="mt-0.5 block truncate text-[12px] text-muted-foreground">
                    {s.summary}
                  </span>
                </span>
              </DropdownMenuItem>
            ))}
            {skills.length > MAX_MENU_SKILLS ? (
              <DropdownMenuItem
                className="gap-2.5 px-2.5 py-2"
                data-testid="composer-all-skills"
                onSelect={() => {
                  after.pending.current = openSkills;
                }}
              >
                <ListChecks className="text-muted-foreground" aria-hidden />
                <span className="flex-1">すべてのスキル（{skills.length}）</span>
                <ChevronRight className="size-4 text-muted-foreground" aria-hidden />
              </DropdownMenuItem>
            ) : null}
            <p className="px-2.5 pb-1 pt-1.5 text-[11.5px] text-muted-foreground">
              入力欄で「/」を打っても呼べます
            </p>
          </>
        ) : null}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

// ── スマホ: 下からのシート ─────────────────────────────────────────

type CreateFn =
  | "createNoteAndOpen"
  | "createDocumentAndOpen"
  | "createSheetAndOpen"
  | "createSlideAndOpen"
  | "createCsvAndOpen";

const CREATE_ITEMS: {
  key: string;
  label: string;
  /// スマホのチップに出す短い名前。
  short: string;
  icon: React.ComponentType<{ className?: string }>;
  iconClass: string;
  fn: CreateFn;
}[] = [
  { key: "note", label: "ノート", short: "ノート", icon: NotebookPen, iconClass: "text-primary", fn: "createNoteAndOpen" },
  {
    key: "document",
    label: "ドキュメント（Word）",
    short: "Word",
    icon: FileText,
    iconClass: "text-blue-600 dark:text-blue-400",
    fn: "createDocumentAndOpen",
  },
  {
    key: "sheet",
    label: "スプレッドシート（Excel）",
    short: "Excel",
    icon: FileSpreadsheet,
    iconClass: "text-emerald-600 dark:text-emerald-400",
    fn: "createSheetAndOpen",
  },
  {
    key: "slide",
    label: "スライド",
    short: "スライド",
    icon: Presentation,
    iconClass: "text-orange-500 dark:text-orange-400",
    fn: "createSlideAndOpen",
  },
  {
    key: "csv",
    label: "CSV（表・SQL 分析）",
    short: "CSV",
    icon: FileSpreadsheet,
    iconClass: "text-green-600 dark:text-green-400",
    fn: "createCsvAndOpen",
  },
];

/// シートの 1 セクション（見出し＋中身）。
export function PlusSection({
  title,
  children,
  className,
}: {
  title: string;
  children: React.ReactNode;
  className?: string;
}) {
  const id = React.useId();
  return (
    <section aria-labelledby={id} className={cn("min-w-0", className)}>
      <h3 id={id} className="mb-2 px-1 text-[12px] font-semibold tracking-wide text-muted-foreground">
        {title}
      </h3>
      {children}
    </section>
  );
}

function PlusSheet({
  open,
  onOpenChange,
  onUploadLocal,
  onOpenDrive,
  createParentId = null,
  skills = [],
  onPickSkill,
  pickSkill,
  modelScope,
  openSkills,
  onOpenModel,
}: SharedProps) {
  const create = useCreateContent({ parentId: createParentId });
  const modelSummary = useModelSummary(modelScope);
  const after = useAfterClose();
  const ranked = useRankedSkills(skills, open);
  const [query, setQuery] = React.useState("");
  React.useEffect(() => {
    if (!open) setQuery("");
  }, [open]);
  const hits = filterSkills(ranked, query).slice(0, query.trim() ? SHEET_SKILLS_SEARCH : SHEET_SKILLS);

  /// シートを閉じてから別の面を開く。
  const thenOpen = (fn: () => void) => {
    after.pending.current = fn;
    onOpenChange(false);
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        className="gap-5 sm:max-w-md"
        data-testid="composer-plus-sheet"
        // 開いた瞬間に検索欄へフォーカスしない（ソフトキーボードがシートを覆う）。
        onOpenAutoFocus={(e) => e.preventDefault()}
        onCloseAutoFocus={after.onCloseAutoFocus}
      >
        <div className="pr-10">
          <DialogTitle className="text-base">追加</DialogTitle>
          <DialogDescription className="sr-only">添付・作成・ツール・スキル</DialogDescription>
        </div>

        {/* ① 添付 */}
        <PlusSection title="添付">
          <div className="grid grid-cols-2 gap-2.5">
            <AttachTile
              icon={ImagePlus}
              label="ファイル・写真"
              hint="この端末から"
              // ファイル選択はタップの操作の中で開く必要がある（閉じてからでは開かない端末がある）。
              // シートは開いたままにし、ファイルを選んだら入力欄側で閉じる（取り消したら残る）。
              onClick={onUploadLocal}
            />
            <AttachTile icon={HardDrive} label="ドライブから" hint="社内のファイル" onClick={() => thenOpen(onOpenDrive)} />
          </div>
        </PlusSection>

        {/* ② 作成（ドライブの「新規作成」と同じ作成関数・作成後は対応エディタへ遷移） */}
        <PlusSection title="作成">
          <div className="-mx-5 flex gap-2 overflow-x-auto overscroll-x-contain px-5 pb-0.5 sm:-mx-6 sm:px-6 [scrollbar-width:none] [&::-webkit-scrollbar]:hidden">
            {CREATE_ITEMS.map((item) => (
              <button
                key={item.key}
                type="button"
                aria-label={`${item.label}を作成`}
                data-testid={`composer-create-${item.key}`}
                onClick={() => {
                  onOpenChange(false);
                  void create[item.fn]();
                }}
                className="inline-flex h-10 shrink-0 items-center gap-2 rounded-full border border-border bg-card px-3.5 text-sm text-foreground/85 transition-colors active:bg-accent"
              >
                <item.icon className={cn("size-4", item.iconClass)} aria-hidden />
                {item.short}
              </button>
            ))}
          </div>
        </PlusSection>

        {/* ③ モデル（入力欄にモデルを出していないときだけ・選ぶと入力欄に出る） */}
        {onOpenModel && modelSummary ? (
          <PlusSection title="モデル">
            <button
              type="button"
              onClick={() => thenOpen(onOpenModel)}
              data-testid="composer-open-model"
              className="flex h-12 w-full items-center gap-3 rounded-xl border border-border bg-card px-3 text-left text-[15px] transition-colors active:bg-accent"
            >
              <Cpu className="size-[18px] shrink-0 text-muted-foreground" aria-hidden />
              <span className="min-w-0 flex-1 truncate">{modelSummary}</span>
              <ChevronRight className="size-4 shrink-0 text-muted-foreground" aria-hidden />
            </button>
          </PlusSection>
        ) : null}

        {/* ④ ツール（#734・この発話にだけ優先して使う） */}
        <ToolChoiceSection scope={modelScope} />

        {/* ⑤ スキル */}
        {skills.length > 0 && onPickSkill ? (
          <PlusSection title="スキル">
            <SkillSearchInput value={query} onChange={setQuery} />
            <div className="mt-1.5">
              {hits.map((s) => (
                <SkillRow
                  key={s.key}
                  skill={s}
                  testId="composer-skill-command"
                  onPick={(picked) => {
                    pickSkill(picked);
                    onOpenChange(false);
                  }}
                />
              ))}
              {query.trim() && hits.length === 0 ? (
                <p className="px-3 py-3 text-sm text-muted-foreground">一致するスキルはありません</p>
              ) : null}
              <button
                type="button"
                onClick={() => thenOpen(openSkills)}
                data-testid="composer-all-skills"
                className="flex h-12 w-full items-center gap-3 rounded-xl px-3 text-left text-[15px] transition-colors active:bg-accent"
              >
                <ListChecks className="size-[18px] shrink-0 text-muted-foreground" aria-hidden />
                <span className="flex-1">すべてのスキル（{skills.length}）</span>
                <ChevronRight className="size-4 shrink-0 text-muted-foreground" aria-hidden />
              </button>
            </div>
            <p className="mt-1 px-1 text-[12px] text-muted-foreground">入力欄で「/」を打っても呼べます</p>
          </PlusSection>
        ) : null}

      </DialogContent>
    </Dialog>
  );
}

function AttachTile({
  icon: Icon,
  label,
  hint,
  onClick,
}: {
  icon: React.ComponentType<{ className?: string }>;
  label: string;
  hint: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="flex h-[84px] flex-col items-start justify-between rounded-2xl border border-border bg-card p-3 text-left transition-colors active:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
    >
      <Icon className="size-5 text-primary" aria-hidden />
      <span>
        <span className="block text-[15px] font-medium leading-tight text-foreground">{label}</span>
        <span className="mt-0.5 block text-[12px] text-muted-foreground">{hint}</span>
      </span>
    </button>
  );
}

// ── ツール（#734） ─────────────────────────────────────────────

/// PC のメニュー内のツール欄（チェック項目。選んでもメニューは閉じない）。
function DesktopToolItems({ scope }: { scope: string }) {
  const { tools, toggle } = useToolChoice(scope);
  return (
    <>
      <DropdownMenuSeparator />
      <DropdownMenuLabel className="uppercase tracking-wide">ツール</DropdownMenuLabel>
      {SELECTABLE_TOOLS.map((t) => (
        <DropdownMenuCheckboxItem
          key={t.name}
          checked={tools.includes(t.name)}
          onCheckedChange={() => toggle(t.name)}
          // 複数選ぶ操作なので、選んでもメニューを閉じない。
          onSelect={(e) => e.preventDefault()}
          className="gap-2.5 py-2"
          data-testid={`composer-tool-${t.name}`}
        >
          <t.icon className="size-4 text-muted-foreground" aria-hidden />
          <span className="min-w-0 flex-1">
            <span className="block truncate text-sm">{t.label}</span>
            <span className="mt-0.5 block truncate text-[12px] text-muted-foreground">{t.hint}</span>
          </span>
        </DropdownMenuCheckboxItem>
      ))}
    </>
  );
}

/// スマホのシートのツール欄（押すたびに付け外しするタイル・複数選べる）。
function ToolChoiceSection({ scope }: { scope: string }) {
  const { tools, toggle } = useToolChoice(scope);
  return (
    <PlusSection title="ツール">
      <div className="grid grid-cols-2 gap-2">
        {SELECTABLE_TOOLS.map((t) => {
          const on = tools.includes(t.name);
          return (
            <button
              key={t.name}
              type="button"
              aria-pressed={on}
              onClick={() => toggle(t.name)}
              data-testid={`composer-tool-${t.name}`}
              className={cn(
                "flex min-h-12 items-center gap-2.5 rounded-xl border px-3 py-2 text-left transition-colors",
                "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
                on
                  ? "border-transparent bg-accent text-foreground"
                  : "border-border bg-card text-foreground/85 active:bg-accent",
              )}
            >
              <t.icon className={cn("size-[18px] shrink-0", on ? "text-primary" : "text-muted-foreground")} aria-hidden />
              <span className="min-w-0 flex-1 text-[14px] font-medium leading-tight">{t.label}</span>
              {on ? <Check className="size-4 shrink-0 text-primary" aria-hidden /> : null}
            </button>
          );
        })}
      </div>
      <p className="mt-1.5 px-1 text-[12px] text-muted-foreground">選んだツールを、この発話で優先して使います</p>
    </PlusSection>
  );
}
