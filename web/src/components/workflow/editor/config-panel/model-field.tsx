"use client";

/// 「AI に聞く」（llm.invoke）のモデル欄（BUG-4・BUG-12・BUG-33）。
///
/// 選べるのは組織の管理者が公開したモデル（`GET /llm/models?purpose=workflow`・保存時の照合と実行時の解決と
/// 同じ一覧で、本人専用のモデルは出ない）。部品はチャットのモデル選択と同じ一覧を使う。
/// ボタンに今のモデル名を出し、押すと一覧を開く。PC はポップオーバー、設定パネルが下からのシートの
/// ときはシートの中に一覧を広げる（シートの上にシートを重ねない）。
///
/// - 一覧の先頭は「組織の既定に従う（現在: …）」の行。選ぶと params から model を外す（省略は実行のたびに
///   組織の既定で解決される・D-5）。新しいブロックもこの状態から始まる
/// - 組織がモデルの選択を禁じている: モデルの行は選べず、既定に従う行だけ（ワークフローでも選ばせない・D-30）。
///   保存済みのモデルが既定と違えば赤字で知らせ、「組織の既定に戻す」を出す
/// - 読み込み中: Skeleton
/// - 一覧を取れない: 自由入力に戻し、注意書きを出す（空欄で組織の既定・保存時の照合はサーバが行う）
/// - 使えるモデルが 0 件: 管理者に相談するよう言う（一覧は編集する人のプロバイダの制限を通すので、組織が公開して
///   いても 0 件になる。どちらの理由でも当たる言い方にする・D-169）。判定用のモデルの欄が空なら「選んでください」の
///   代わりにこの文を誤りとして出す（選べるものが無い）
/// - 保存済みのモデルが一覧に無い: 「このモデルは使えません: X」を赤字で出す（値は消さない）
///
/// 「AI で判定」（ai.decide）は `modality="decision"` で使う。一覧は判定用のモデルだけ
/// （`GET /llm/models?purpose=workflow&modality=decision`・保存時の照合と同じ集合）で、判定には組織の既定が
/// 無いので既定の行を出さず、必ず選ばせる。組織がモデルの選択を禁じていても判定のモデルには掛けない
/// （ir.md §7.11）。
///
/// 欄の下の「詳しく」で、入れた内容がどの提供元に送られるかを言う（提供元の表示名と国外処理の印は一覧から・D-99）。

import * as React from "react";
import { ChevronDown } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { Skeleton } from "@/components/ui/skeleton";
import { EFFORT_LABEL } from "@/components/admin/format";
import { AbroadBadge, ModelList, type ModelListDefaultRow } from "@/components/llm/model-list";
import { useMediaQuery } from "@/hooks/use-media-query";
import type { LlmModelsResponse, ModelModality } from "@/lib/admin-ai-api";
import { useLlmModelsState } from "@/lib/model-choice";
import { providerOf } from "@/lib/model-provider";
import { cn } from "@/lib/utils";
import { Field, MoreInfo } from "./common";

/// 設定パネルがシートになる幅（config-panel/index.tsx と同じ）。
const PANEL_QUERY = "(min-width: 768px)";

const LOCKED_NOTE = "この組織ではモデルを選べません（管理者の設定）";

/// 組織の既定で今動くモデルの表示（Auto なら振り先と思考の深さ・チャットの「Auto」の行と同じ書き方）。
export function orgDefaultLabel(data: LlmModelsResponse): string {
  if (data.default_model !== "auto") {
    return data.models.find((m) => m.id === data.default_model)?.label ?? data.default_model;
  }
  return data.auto ? `Auto（${data.auto.label}・${EFFORT_LABEL[data.auto.effort]}）` : "Auto";
}

function defaultRow(data: LlmModelsResponse): ModelListDefaultRow {
  return {
    label: "組織の既定に従う",
    target: `現在: ${orgDefaultLabel(data)}`,
    keywords: "既定 組織 auto おまかせ",
  };
}

export function ModelField({
  value,
  onChange,
  error,
  modality = "chat",
}: {
  value: string | undefined;
  onChange: (next: string | undefined) => void;
  /// サーバの検証の誤り（`/params/model`）。
  error?: string | null;
  /// `decision` は判定用のモデルの欄（既定の行なし・必ず選ぶ・組織の選択の禁止は掛けない）。
  modality?: ModelModality;
}) {
  const decision = modality === "decision";
  const label = decision ? "判定用のモデル" : "モデル";
  const state = useLlmModelsState("workflow", modality);
  const desktop = useMediaQuery(PANEL_QUERY);
  const [open, setOpen] = React.useState(false);
  const listRef = React.useRef<HTMLDivElement | null>(null);

  if (state.status === "loading") {
    return (
      <Field label={label}>
        <div aria-busy="true" aria-label="モデルの一覧を読み込み中">
          <Skeleton className="h-8 w-full" />
        </div>
      </Field>
    );
  }

  if (state.status === "error" || state.data.models.length === 0) {
    const hint = decision
      ? state.status === "error"
        ? "判定用のモデルの一覧を読み込めませんでした。管理者が登録した判定用のモデルの ID をそのまま書いてください"
        : "使える判定用のモデルがありません。管理者に相談してください"
      : state.status === "error"
        ? "モデルの一覧を読み込めませんでした。空欄なら組織の既定で動きます。指定するときは管理者が登録したモデル ID をそのまま書いてください"
        : "使えるモデルがありません。管理者に相談してください";
    // 判定用のモデルの欄が空のとき、選べるモデルが 0 件なのに「選んでください」と言っても直せないので、0 件の文を
    // 誤りとして出す（判定はモデルが無いと動かない・D-169）。
    const shown = decision && !value && state.status !== "error" ? hint : error;
    return (
      <Field label={label} error={shown} hint={hint}>
        <Input
          value={value ?? ""}
          onChange={(e) => onChange(e.target.value || undefined)}
          placeholder={decision ? "判定用のモデルの ID" : "空欄で組織の既定"}
          aria-label="モデル ID"
          aria-invalid={shown ? true : undefined}
          className="h-8 font-mono text-xs max-md:text-base"
        />
      </Field>
    );
  }

  const data = state.data;
  // 判定のモデルには組織の選択の禁止を掛けない（判定には組織の既定が無い・ir.md §7.11）。
  const locked = !decision && !data.allow_user_choice;
  const current = value ? data.models.find((m) => m.id === value) : undefined;
  // 選択を禁じた組織では、既定と同じモデル（を書いた保存済みの定義）だけが通る（保存時の照合と同じ）。
  const lockedOut = locked && value !== undefined && value !== data.default_model;
  // サーバの誤りがあればそちらを出す（使えるモデルの一覧も添えてある）。無ければ手元の一覧で先に知らせる。
  const message =
    error ??
    (lockedOut
      ? "この組織ではモデルを選べません。「組織の既定に従う」にしてください"
      : value && !current
        ? `このモデルは使えません: ${value}`
        : null);
  const pick = (id: string | null) => {
    onChange(id ?? undefined);
    setOpen(false);
  };
  const shown =
    value === undefined || value === ""
      ? decision
        ? "選んでください"
        : `組織の既定（${orgDefaultLabel(data)}）`
      : (current?.label ?? value);
  const hint = decision
    ? current?.description || "管理者が判定用として使えるようにしたモデルから選びます"
    : locked
      ? `${LOCKED_NOTE}。組織の既定で動きます`
      : value === undefined
        ? "省略すると組織の既定で動きます。管理者が既定を変えると、このブロックのモデルも変わります"
        : current?.description || "管理者が使えるようにしたモデルから選びます";

  const button = (
    <button
      type="button"
      aria-label={`${label}: ${shown}（変更する）`}
      aria-expanded={open}
      data-testid="workflow-model-button"
      onClick={desktop ? undefined : () => setOpen((o) => !o)}
      className={cn(
        "flex h-8 w-full items-center gap-2 rounded-md border border-input bg-background px-3 text-left text-sm shadow-xs pointer-coarse:h-11",
        "transition-colors hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
        message && "border-destructive",
      )}
    >
      <span className={cn("min-w-0 flex-1 truncate", !value && "text-muted-foreground")}>{shown}</span>
      {current ? <span className="shrink-0 text-[11px] text-muted-foreground">{current.provider_label}</span> : null}
      <ChevronDown className={cn("size-3.5 shrink-0 text-muted-foreground transition-transform", open && "rotate-180")} aria-hidden />
    </button>
  );
  const list = (surface: "popover" | "sheet") => (
    <>
      {locked ? (
        <p className="shrink-0 px-2.5 pb-1 pt-1.5 text-[11.5px] text-muted-foreground" data-testid="workflow-model-locked">
          {LOCKED_NOTE}
        </p>
      ) : null}
      <ModelList
        data={data}
        // 一覧に無いモデルを保存しているときは、どの行も選択中にしない（既定の行を選択中に見せない）。
        current={value || null}
        onPick={pick}
        surface={surface}
        listRef={listRef}
        defaultRow={decision ? null : defaultRow(data)}
        modelsDisabled={locked}
      />
    </>
  );

  return (
    <div className="space-y-1">
      <Field label={label} error={message} hint={hint}>
        {desktop ? (
          <Popover open={open} onOpenChange={setOpen}>
            <PopoverTrigger asChild>{button}</PopoverTrigger>
            <PopoverContent
              align="end"
              collisionPadding={12}
              className="flex max-h-[min(var(--radix-popover-content-available-height),24rem)] w-80 flex-col p-1.5"
              data-testid="workflow-model-picker"
            >
              {list("popover")}
            </PopoverContent>
          </Popover>
        ) : (
          <>
            {button}
            {open ? <InlineList onClose={() => setOpen(false)}>{list("sheet")}</InlineList> : null}
          </>
        )}
        {lockedOut ? (
          <Button type="button" variant="outline" size="sm" onClick={() => onChange(undefined)}>
            組織の既定に戻す
          </Button>
        ) : null}
      </Field>
      <ProviderInfo data={data} value={value} />
    </div>
  );
}

/// 入れた内容の送り先（D-99）。選んだモデル（選ばなければ組織の既定）の提供元を一覧から引く。一覧に無いモデルは
/// 提供元を言わない（欄の下の誤りが先に出る）。
function ProviderInfo({ data, value }: { data: LlmModelsResponse; value: string | undefined }) {
  const provider = providerOf(data, value);
  return (
    <MoreInfo testId="workflow-model-provider">
      <p className="flex flex-wrap items-center gap-x-1.5 gap-y-1">
        <span>
          入れた内容は、組織の AI の設定で決めたモデルの提供元に送られます
          {provider ? `（いま: ${provider.label}）` : ""}。
        </span>
        {provider?.abroad ? <AbroadBadge /> : null}
      </p>
    </MoreInfo>
  );
}

/// シートの中に広げる一覧の枠。開いたら一覧（または検索欄）へフォーカスを移す。
function InlineList({ children, onClose }: { children: React.ReactNode; onClose: () => void }) {
  const ref = React.useRef<HTMLDivElement | null>(null);
  React.useEffect(() => {
    // ソフトキーボードを出さないよう、検索欄ではなく一覧へ移す（チャットのシートと同じ）。
    ref.current?.querySelector<HTMLElement>('[role="listbox"]')?.focus({ preventScroll: true });
    ref.current?.scrollIntoView({ block: "nearest" });
  }, []);
  return (
    <div
      ref={ref}
      className="flex max-h-72 flex-col rounded-md border bg-background p-1"
      data-testid="workflow-model-picker"
      // 一覧の中の Esc は一覧だけを閉じる（シートごと閉じない・local-escape.ts）。
      data-local-escape="true"
      onKeyDown={(e) => {
        if (e.key === "Escape") {
          e.stopPropagation();
          onClose();
        }
      }}
    >
      {children}
    </div>
  );
}
