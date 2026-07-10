"use client";

/// 外部連携ノードのフォーム（外部 API 呼び出し・スクリプト）。

import * as React from "react";

import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import type { NodeParamsByType } from "@/generated/workflow-catalog";
import type { ValueExpr } from "@/generated/workflow-ir";
import { Field } from "./common";
import { paramsOf, patchParams } from "./params";
import { ValueExprInput } from "./value-expr-input";
import type { FormProps } from "./forms-basic";

const METHODS = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"] as const;

export function HttpRequestForm({ node, dispatch, refCandidates, inMapRegion, fieldErrors }: FormProps) {
  const p = paramsOf<NodeParamsByType["http.request"]>(node);
  const secret = p.secret as { name?: string; attach?: { kind?: string; header?: string } } | undefined;
  return (
    <div className="space-y-3">
      <div className="grid grid-cols-[6.5rem_1fr] gap-2">
        <Field label="メソッド">
          <Select
            value={(p.method as string | undefined) ?? "GET"}
            onValueChange={(v) => patchParams(dispatch, node, { method: v })}
          >
            <SelectTrigger className="h-8">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {METHODS.map((m) => (
                <SelectItem key={m} value={m}>
                  {m}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </Field>
        <Field
          label="URL"
          hint="ドメインは固定文字列で入力（安全のため差し込み不可）"
          error={fieldErrors.get("url")}
        >
          <Input
            value={(p.url as string | undefined) ?? ""}
            onChange={(e) => patchParams(dispatch, node, { url: e.target.value })}
            placeholder="https://api.example.com/v1"
            className="h-8 font-mono text-xs"
          />
        </Field>
      </div>
      <ValueExprInput
        label="URL の続き（省略可）"
        value={p.path_suffix as ValueExpr | undefined}
        onChange={(v) => patchParams(dispatch, node, { path_suffix: v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
        placeholder="例: /items/123"
        error={fieldErrors.get("path_suffix")}
      />
      <ValueExprInput
        label="送る内容（省略可）"
        value={p.body as ValueExpr | undefined}
        onChange={(v) => patchParams(dispatch, node, { body: v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
        multiline
        error={fieldErrors.get("body")}
      />
      <Field
        label="使用するシークレット（省略可）"
        hint="登録済みシークレットの参照名。宛先が URL と一致しないと保存できません"
        error={fieldErrors.get("secret")}
      >
        <div className="space-y-1.5">
          <Input
            value={secret?.name ?? ""}
            onChange={(e) =>
              patchParams(dispatch, node, {
                secret: e.target.value
                  ? { ...(secret ?? {}), name: e.target.value }
                  : undefined,
              })
            }
            placeholder="例: slack-bot-token"
            className="h-8 font-mono text-xs"
          />
          {secret?.name ? (
            <div className="grid grid-cols-2 gap-1.5">
              <Select
                value={secret.attach?.kind ?? "bearer"}
                onValueChange={(v) =>
                  patchParams(dispatch, node, {
                    secret: {
                      ...secret,
                      attach: v === "bearer" ? { kind: "bearer" } : { kind: "header", header: secret.attach?.header ?? "X-Api-Key" },
                    },
                  })
                }
              >
                <SelectTrigger className="h-8">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="bearer">Bearer 認証</SelectItem>
                  <SelectItem value="header">ヘッダに添付</SelectItem>
                </SelectContent>
              </Select>
              {secret.attach?.kind === "header" ? (
                <Input
                  value={secret.attach?.header ?? ""}
                  onChange={(e) =>
                    patchParams(dispatch, node, {
                      secret: {
                        ...secret,
                        attach: { kind: "header", header: e.target.value },
                      },
                    })
                  }
                  placeholder="ヘッダ名"
                  className="h-8 font-mono text-xs"
                />
              ) : null}
            </div>
          ) : null}
        </div>
      </Field>
      <p className="text-[11px] text-muted-foreground">
        外部への送信はやり直しで二重になることがあります（Idempotency-Key を自動付与）
      </p>
    </div>
  );
}

export function ScriptRunForm({ node, dispatch, refCandidates, inMapRegion, fieldErrors }: FormProps) {
  const p = paramsOf<NodeParamsByType["script.run"]>(node);
  const source = p.source as { inline?: string } | undefined;
  return (
    <div className="space-y-3">
      <Field
        label="スクリプト（TypeScript）"
        hint="main(input) の戻り値が次のブロックへ渡ります"
        error={fieldErrors.get("source")}
      >
        <Textarea
          value={source?.inline ?? ""}
          rows={10}
          spellCheck={false}
          onChange={(e) =>
            patchParams(dispatch, node, { source: { inline: e.target.value } })
          }
          className="font-mono text-xs leading-5"
        />
      </Field>
      <ValueExprInput
        label="渡す入力（省略可）"
        value={p.input as ValueExpr | undefined}
        onChange={(v) => patchParams(dispatch, node, { input: v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
      />
    </div>
  );
}
