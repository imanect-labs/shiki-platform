"use client";

/// 外部連携ノードのフォーム（外部 API 呼び出し・スクリプト）。

import * as React from "react";

import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  HTTP_MAX_HEADERS,
  HTTP_MAX_HEADER_VALUE_BYTES,
  type NodeParamsByType,
} from "@/generated/workflow-catalog";
import type { ValueExpr } from "@/generated/workflow-ir";
import { Field, FieldInput, FieldSelectTrigger, FieldTextarea, MoreInfo } from "./common";
import { paramsOf, patchParams } from "./params";
import { ValueExprInput } from "./value-expr-input";
import type { FormProps } from "./forms-basic";
import { ObjectComposer, type ComposeNouns } from "./value-compose";
import { ANY_SHAPES } from "./value-shape";

const METHODS = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"] as const;

/// 「ヘッダー」の行の呼び名（GAP-19）。
const HEADER_NOUNS: ComposeNouns = {
  key: "ヘッダーの名前",
  keyPlaceholder: "ヘッダーの名前（例: X-Request-Id）",
  add: "ヘッダーを追加",
  empty: "「ヘッダーを追加」で、ヘッダーの名前と値の組を足します。",
  issues: {
    blank_key: "ヘッダーの名前を入れてください。",
    duplicate: "同じ名前のヘッダーがほかにもあります。別の名前にしてください。",
    no_value: "値を入れるまで、このヘッダーは保存しません。",
  },
};

/// 「ヘッダー」の欄（名前と値の組・値は差し込める・GAP-19）。決まり（書けないヘッダー・上限）は保存時にサーバが
/// 確かめ、誤りは欄の下に出す（nodes/http_headers.rs が正。画面では決まりを写さない）。
function HttpHeadersField({ node, dispatch, refCandidates, inMapRegion, fieldErrors }: FormProps) {
  const p = paramsOf<NodeParamsByType["http.request"]>(node);
  return (
    <div className="space-y-1">
      <Field
        label="ヘッダー（省略可）"
        hint="書いたヘッダーを付けて送ります。値には、入力や前のブロックの結果を差し込めます。"
        error={fieldErrors.get("headers")}
      >
        <ObjectComposer
          value={p.headers as ValueExpr | undefined}
          // 行をすべて外したら、欄ごと消す（ヘッダーを書かない定義と同じ形に戻す）。
          onChange={(v) =>
            patchParams(dispatch, node, {
              headers: v && typeof v === "object" && Object.keys(v).length > 0 ? v : undefined,
            })
          }
          depth={0}
          nouns={HEADER_NOUNS}
          renderSlot={(slot) => (
            <ValueExprInput
              label={slot.label}
              shownLabel={slot.shownLabel}
              value={slot.value}
              onChange={slot.onChange}
              depth={slot.depth}
              actions={slot.actions}
              autoFocus={slot.autoFocus}
              text
              refCandidates={refCandidates}
              inMapRegion={inMapRegion}
              placeholder="例: application/json"
            />
          )}
        />
      </Field>
      <MoreInfo testId="http-headers-rule">
        <p>
          Host や Content-Length など、Shiki が決めるヘッダーは書けません。シークレットを付けるヘッダーと同じ名前も書けません。
        </p>
        <p>
          {HTTP_MAX_HEADERS} 個まで、値は 1 つ {HTTP_MAX_HEADER_VALUE_BYTES / 1024} KB
          までです。Idempotency-Key や Content-Type を書くと、Shiki が付ける値の代わりに、書いた値を送ります。
        </p>
      </MoreInfo>
    </div>
  );
}

export function HttpRequestForm(props: FormProps) {
  const { node, dispatch, refCandidates, inMapRegion, fieldErrors } = props;
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
            <FieldSelectTrigger className="h-8">
              <SelectValue />
            </FieldSelectTrigger>
            <SelectContent>
              {METHODS.map((m) => (
                <SelectItem key={m} value={m}>
                  {m}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </Field>
        <div className="min-w-0 space-y-1">
          <Field
            label="URL"
            hint="https:// から書きます。安全のため値は差し込めないので、変わる部分は「URL の続き」に書きます。送れる先は、管理者とシークレットの設定で決まります。"
            error={fieldErrors.get("url")}
          >
            <FieldInput
              value={(p.url as string | undefined) ?? ""}
              onChange={(e) => patchParams(dispatch, node, { url: e.target.value })}
              placeholder="https://api.example.com/v1"
              className="h-8 font-mono text-xs max-md:text-base"
            />
          </Field>
          <MoreInfo testId="http-destination-rule">
            {/* 送れる先の照合は nodes/capability_ai.rs の check_http_destination（管理者の許可とシークレットの送り先の AND）。 */}
            <p>シークレットを使わないときは、管理者が許可した送り先にだけ送れます。管理者が送り先を許可していなければ、シークレットを使わない送信はできません。</p>
            <p>シークレットを使うときは、そのシークレットに決めた送り先にだけ送れます。管理者が送り先を決めているときは、その中に限ります。</p>
          </MoreInfo>
        </div>
      </div>
      <ValueExprInput
        label="URL の続き（省略可）"
        text
        value={p.path_suffix as ValueExpr | undefined}
        onChange={(v) => patchParams(dispatch, node, { path_suffix: v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
        placeholder="例: /items/123"
        error={fieldErrors.get("path_suffix")}
      />
      <ValueExprInput
        label="送る内容（省略可）"
        text
        shapes={ANY_SHAPES}
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
          <FieldInput
            value={secret?.name ?? ""}
            onChange={(e) =>
              patchParams(dispatch, node, {
                secret: e.target.value
                  ? { ...(secret ?? {}), name: e.target.value }
                  : undefined,
              })
            }
            placeholder="例: slack-bot-token"
            className="h-8 font-mono text-xs max-md:text-base"
          />
          {secret?.name ? (
            <div className="space-y-1.5">
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
                <SelectTrigger className="h-8" aria-label="シークレットの付け方">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="bearer">Authorization ヘッダーに Bearer で付ける（おすすめ）</SelectItem>
                  <SelectItem value="header">決めたヘッダーに付ける</SelectItem>
                </SelectContent>
              </Select>
              {secret.attach?.kind === "header" ? (
                <Input
                  value={secret.attach?.header ?? ""}
                  aria-label="シークレットを付けるヘッダーの名前"
                  onChange={(e) =>
                    patchParams(dispatch, node, {
                      secret: {
                        ...secret,
                        attach: { kind: "header", header: e.target.value },
                      },
                    })
                  }
                  placeholder="例: X-Api-Key"
                  className="h-8 font-mono text-xs max-md:text-base"
                />
              ) : null}
            </div>
          ) : null}
        </div>
      </Field>
      <HttpHeadersField {...props} />
      <label className="flex items-center justify-between gap-3 text-xs">
        <span>
          <span className="font-medium">相手が誤りを返したら失敗にする</span>
          <span className="block text-[11px] leading-relaxed text-muted-foreground" data-testid="http-fail-on-error-note">
            {p.fail_on_error_status
              ? "相手が誤り（400 以上）を返すと、このブロックは失敗し、「失敗したとき」の設定に従います。"
              : "相手が誤り（400 以上）を返しても、このブロックは失敗になりません。結果の応答のステータスコード（status）で分けてください。"}
          </span>
        </span>
        <Switch
          aria-label="相手が誤りを返したら失敗にする"
          checked={Boolean(p.fail_on_error_status)}
          onCheckedChange={(on) => patchParams(dispatch, node, { fail_on_error_status: on || undefined })}
        />
      </label>
      {p.fail_on_error_status ? (
        <MoreInfo>
          <p>
            400〜499（408・425・429 を除く）は、送る内容か設定を直すまで通らないので、やり直しません。500
            以上と 408・425・429 は一時的な誤りとして、「試す回数」が 2 以上ならやり直します。
          </p>
        </MoreInfo>
      ) : null}
      <p className="text-[11px] leading-relaxed text-muted-foreground">
        失敗してやり直すと、相手に同じ内容が 2 回届くことがあります。重複を見分ける印（Idempotency-Key）を付けて送ります。
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
        <FieldTextarea
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
        shapes={ANY_SHAPES}
        value={p.input as ValueExpr | undefined}
        onChange={(v) => patchParams(dispatch, node, { input: v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
      />
    </div>
  );
}
