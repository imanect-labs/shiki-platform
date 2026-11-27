"use client";

/// 入力項目の編集と、その保存時の誤り（GAP-35）。
///
/// 「手動で実行」のきっかけの設定と、きっかけと別に開く入力項目の設定（画面の上の「入力項目」と「直すところ」の一覧から
/// 開く）の両方に置く。入力項目はワークフローに 1 つで、「実行」ボタン・別のワークフローからの実行・アプリのボタンで
/// 共通なので、手動で実行のきっかけが無いワークフロー（別のワークフローから呼ばれるものなど）でも直せるようにする。

import { AlertCircle } from "lucide-react";

import type { ValidationError } from "@/generated/workflow-ir";
import { InputSchemaEditor, type InputRefUsage } from "./input-schema-editor";

export function InputsSection({
  schema,
  onChange,
  usages,
  errors = [],
  title = true,
}: {
  schema: unknown;
  onChange: (next: unknown, renames?: Record<string, string>) => void;
  usages?: InputRefUsage[];
  /// 入力項目の保存時の誤り（編集の下に出す）。
  errors?: ValidationError[];
  /// 見出し「入力項目」を出すか。
  title?: boolean;
}) {
  return (
    <>
      <InputSchemaEditor schema={schema} onChange={onChange} usages={usages} title={title} />
      {errors.length > 0 ? (
        <div
          role="alert"
          className="mt-2 space-y-1 rounded-lg border border-destructive/40 bg-destructive/5 px-3 py-2 text-xs text-destructive"
          data-testid="input-schema-errors"
        >
          <p className="font-medium">入力項目を直すまで保存できません</p>
          {errors.map((e, i) => (
            <p key={i} className="flex items-start gap-1.5 [overflow-wrap:anywhere]">
              <AlertCircle className="mt-0.5 size-3.5 shrink-0" aria-hidden />
              {e.message}
            </p>
          ))}
        </div>
      ) : null}
    </>
  );
}
