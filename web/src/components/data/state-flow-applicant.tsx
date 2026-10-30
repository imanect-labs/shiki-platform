"use client";

/// 「状態の進め方」の画面の「申請した本人の扱い」（D-115・D-122）: 申請者の列と、最初の状態を過ぎたら申請者が
/// 書き換えられない決まり。
///
/// - 申請者の列は作成者の列（レコードを追加した人が自動で入り、だれも書き換えられない）を勧める。メンバーの列は
///   テーブルを編集できる人が書き換えられるので、申請者の列をほかの人にすると自分の申請でも承認できる（BUG-148）。
/// - 作成者の列が無いテーブルでは、ボタン 1 つで足せる（列を足すのはすぐ保存する。状態の進め方の保存は別）。
/// - メンバーの列で判定している保存済みの進め方は、そのまま動く。作成者の列に替えるよう案内するだけで、勝手に替えない。

import { Loader2, Plus, Share2 } from "lucide-react";
import * as React from "react";

import { MoreInfo } from "@/components/workflow/editor/config-panel/common";
import { Button } from "@/components/ui/button";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import type { FieldDef } from "@/lib/data-api";
import { isApplicantColumn, type FlowDraft } from "@/lib/state-flow";
import { cn } from "@/lib/utils";

const labelOf = (f: FieldDef) => f.display_name || f.key;

function Hint({ children, tone, testId }: { children: React.ReactNode; tone?: "warn"; testId?: string }) {
  return (
    <p
      className={cn("text-[11px] leading-relaxed", tone === "warn" ? "text-[var(--season-autumn)]" : "text-muted-foreground")}
      data-testid={testId}
    >
      {children}
    </p>
  );
}

export function ApplicantSection({
  draft,
  fields,
  initialName,
  onChange,
  onAddCreator,
  adding,
  addError,
  onOpenSchema,
  onOpenShare,
}: {
  draft: FlowDraft;
  fields: FieldDef[];
  /// 最初の状態の名前（文の中で使う）。
  initialName: string;
  onChange: (patch: Partial<FlowDraft>) => void;
  /// 作成者の列を足して、申請者の列にする（無ければボタンを出さない）。
  onAddCreator?: () => void;
  adding: boolean;
  addError: string | null;
  /// 「列の編集」を開く（メンバーの列などを自分で足すとき）。
  onOpenSchema?: () => void;
  onOpenShare?: () => void;
}) {
  const candidates = fields.filter(isApplicantColumn);
  const creator = fields.find((f) => f.type === "created_by") ?? null;
  const selected = fields.find((f) => f.key === draft.applicantField) ?? null;
  const byCreator = selected?.type === "created_by";
  const byMember = selected?.type === "user_ref";

  /// 作成者の列に替える・足すボタン（作成者の列で判定していないときだけ）。
  const switchButton = byCreator ? null : creator ? (
    <Button
      size="sm"
      variant="outline"
      className="h-7 gap-1 text-xs pointer-coarse:h-9"
      onClick={() => onChange({ applicantField: creator.key })}
      data-testid="state-flow-use-creator"
    >
      「{labelOf(creator)}」に替える
    </Button>
  ) : onAddCreator ? (
    <Button
      size="sm"
      variant="outline"
      className="h-7 gap-1 text-xs pointer-coarse:h-9"
      onClick={onAddCreator}
      disabled={adding}
      data-testid="state-flow-add-creator"
    >
      {adding ? <Loader2 className="size-3.5 animate-spin" aria-hidden /> : <Plus className="size-3.5" aria-hidden />}
      {selected ? "作成者の列を追加して替える" : "作成者の列を追加"}
    </Button>
  ) : null;

  return (
    <>
      {candidates.length === 0 ? (
        <div className="space-y-2 rounded-lg border border-dashed p-3" data-testid="state-flow-no-applicant">
          <Hint tone="warn">申請した本人も、自分の申請を承認できます。</Hint>
          <Hint>
            作成者の列を追加すると、レコードを追加した人を、承認する人から除けます。作成者の列には追加した人が自動で入り、だれも書き換えられません。
          </Hint>
          <div className="flex flex-wrap gap-2">
            {switchButton}
            {onOpenSchema ? (
              <Button size="sm" variant="ghost" className="h-7 text-xs pointer-coarse:h-9" onClick={onOpenSchema} data-testid="state-flow-open-schema">
                列の編集を開く
              </Button>
            ) : null}
          </div>
        </div>
      ) : (
        <>
          <Select value={draft.applicantField ?? "__none"} onValueChange={(k) => onChange({ applicantField: k === "__none" ? null : k })}>
            <SelectTrigger aria-label="申請者の列" className="h-9 sm:w-72">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="__none">使わない</SelectItem>
              {candidates.map((f) => (
                <SelectItem key={f.key} value={f.key}>
                  {labelOf(f)}
                  {f.type === "created_by" ? "（作成者の列）" : "（メンバーの列）"}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          {byCreator ? (
            <div className="space-y-1" data-testid="state-flow-applicant-creator">
              <Hint>
                「{labelOf(selected)}」には、レコードを追加した人が自動で入ります。だれも書き換えられないので、申請した本人を承認する人から確実に除けます。
              </Hint>
              <MoreInfo testId="state-flow-applicant-more">
                <p>
                  代わりに申請する業務（レコードを追加する人と、申請する人が違う）では、除かれるのはレコードを追加した人です。申請者として名前を入れた人は、自分の申請を承認できます。
                </p>
                <p>
                  ワークフローの自動実行でレコードを追加するときも同じです。追加したのはワークフローになるので、申請した人は除かれません。
                </p>
              </MoreInfo>
            </div>
          ) : byMember ? (
            <div className="space-y-1" data-testid="state-flow-applicant-member">
              <Hint>申請した人が入るメンバーの列です。申請した本人を、承認する人から除くのに使います。</Hint>
              <Hint tone="warn" testId="state-flow-member-bypass">
                テーブルを編集できる人は、追加するときと「{initialName}」のあいだに、申請者の列をほかの人にできます。そうすると、自分の申請でも承認できます。作成者の列に替えると防げます。
              </Hint>
            </div>
          ) : (
            <Hint tone="warn">申請者の列を選ばないと、申請した本人も自分の申請を承認できます。</Hint>
          )}
          {switchButton}
          {selected ? (
            <>
              <label className="flex items-center gap-2 text-[13px]">
                <Switch
                  checked={draft.writeLock}
                  onCheckedChange={(writeLock) => onChange({ writeLock })}
                  aria-label="最初の状態を過ぎたら、申請者は書き換えられない"
                  data-testid="state-flow-write-lock"
                />
                最初の状態を過ぎたら、申請者は書き換えられない
              </label>
              <Hint tone={draft.writeLock ? undefined : "warn"}>
                {draft.writeLock
                  ? byCreator
                    ? `「${initialName}」のあいだだけ、レコードを追加した人はレコードを書き換えたり削除したりできます。`
                    : `「${initialName}」のあいだだけ、申請者はレコードを書き換えたり削除したりできます。申請者の列が空のレコードは、「${initialName}」を過ぎると誰も書き換えられません。`
                  : byCreator
                    ? "レコードを追加した人は、承認された後もレコードを書き換えられます。"
                    : "申請者は、承認された後もレコードを書き換えられます。承認者の列を書き換えて、自分で承認することもできます。"}
              </Hint>
            </>
          ) : null}
        </>
      )}
      {addError ? (
        <p className="text-[11px] text-destructive" role="alert" data-testid="state-flow-add-creator-error">
          {addError}
        </p>
      ) : null}
      <div className="space-y-1.5 rounded-lg bg-muted/50 px-3 py-2">
        <Hint>
          申請する人には、テーブルを「追加だけ」で共有するのがおすすめです。追加だけの人は、レコードを追加し、自分が追加したレコードと個別に共有されたレコードを見られるだけで、書き換えも状態を進めることもできません。
        </Hint>
        {onOpenShare ? (
          <Button size="sm" variant="outline" className="h-7 gap-1 text-xs" onClick={onOpenShare}>
            <Share2 className="size-3.5" aria-hidden />
            テーブルを共有
          </Button>
        ) : null}
      </div>
    </>
  );
}
