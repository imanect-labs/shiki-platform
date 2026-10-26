"use client";

/// 「AI エージェント」（agent.invoke）の設定フォーム（BUG-6）。
///
/// エージェントは依頼（自然文）を、選んだ道具で調べながら果たし、最後の回答を出力の `/text` に書く。
/// 欄は codegen の typed 契約（`AgentInvokeParams`）と、道具のカタログ（`AGENT_TOOL_CATALOG`・名前・説明・
/// 要るスコープ・社外へ届くか）に対応する。効かない設定は出さない。
///
/// - 道具を選ばなければ道具なしで答える（保存すると選んだ道具の要るスコープが宣言に補われる）
/// - 「読んでよいサイト」は Web ページを読む道具を選んだときだけ、管理者が許可したサイトから選ぶ
///   （自由入力にしない・`GET /workflows/agent-settings`）。管理者が許可していなければ道具を選べない
/// - サイトの一覧の読み込み中は Skeleton、取れないときは Web ページを読む道具を選べないと出す

import * as React from "react";

import { Badge } from "@/components/ui/badge";
import { Skeleton } from "@/components/ui/skeleton";
import { AGENT_TOOL_CATALOG, type NodeParamsByType } from "@/generated/workflow-catalog";
import type { AgentTool, ValueExpr } from "@/generated/workflow-ir";
import { getAgentSettings, type AgentSettings } from "@/lib/workflow-api";
import { scopeLabel } from "@/lib/workflow-scopes";
import { cn } from "@/lib/utils";
import { Field, NumberInput } from "./common";
import type { FormProps } from "./forms-basic";
import { ModelField } from "./model-field";
import { paramsOf, patchParams } from "./params";
import { ValueExprInput } from "./value-expr-input";

type SettingsState =
  | { status: "loading" }
  | { status: "error" }
  | { status: "ready"; data: AgentSettings };

function useAgentSettings(): SettingsState {
  const [state, setState] = React.useState<SettingsState>({ status: "loading" });
  React.useEffect(() => {
    let alive = true;
    getAgentSettings()
      .then((data) => alive && setState({ status: "ready", data }))
      .catch(() => alive && setState({ status: "error" }));
    return () => {
      alive = false;
    };
  }, []);
  return state;
}

const TOOLS = AGENT_TOOL_CATALOG.filter((t) => t.available);

const CHECK_ROW =
  "flex min-h-9 cursor-pointer items-start gap-2 rounded-md px-1.5 py-1 text-sm pointer-coarse:min-h-11 has-[:disabled]:cursor-not-allowed has-[:focus-visible]:ring-2 has-[:focus-visible]:ring-ring";

export function AgentInvokeForm({ node, dispatch, refCandidates, inMapRegion, fieldErrors }: FormProps) {
  const p = paramsOf<NodeParamsByType["agent.invoke"]>(node);
  const settings = useAgentSettings();
  const tools = (p.allowed_tools as AgentTool[] | undefined) ?? [];
  const hosts = (p.egress_allowlist as string[] | undefined) ?? [];
  const ceiling = settings.status === "ready" ? settings.data.web_allowlist : [];
  const fetchOn = tools.includes("web.fetch");
  const fetchBlocked = settings.status !== "ready" || ceiling.length === 0;

  const toggleTool = (tool: AgentTool, on: boolean) => {
    const next = on ? [...tools.filter((t) => t !== tool), tool] : tools.filter((t) => t !== tool);
    // Web ページを読む道具を外したら、読んでよいサイトも外す（効かない設定を残さない）。
    const patch: Record<string, unknown> = { allowed_tools: next.length > 0 ? next : undefined };
    if (tool === "web.fetch" && !on) patch.egress_allowlist = [];
    patchParams(dispatch, node, patch);
  };
  const toggleHost = (host: string, on: boolean) =>
    patchParams(dispatch, node, {
      egress_allowlist: on ? [...hosts.filter((h) => h !== host), host] : hosts.filter((h) => h !== host),
    });
  // 天井から外れた保存済みのサイトも見せる（外せるように・保存時の照合が欄の下に誤りを出す）。
  // 道具を選んでいなければ、保存済みのサイト（旧い定義の名残り）だけを見せる。
  const hostOptions = fetchOn ? [...ceiling, ...hosts.filter((h) => !ceiling.includes(h))] : hosts;

  return (
    <div className="space-y-3">
      <ValueExprInput
        label="エージェントへの依頼"
        text
        value={p.instruction as ValueExpr | undefined}
        onChange={(v) => patchParams(dispatch, node, { instruction: v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
        multiline
        placeholder="例: 社内の旅費規程を調べて、日当の金額と対象になる条件をまとめて"
        error={fieldErrors.get("instruction")}
      />
      <ValueExprInput
        label="参考データ（省略可）"
        value={p.context as ValueExpr | undefined}
        onChange={(v) => patchParams(dispatch, node, { context: v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
        error={fieldErrors.get("context")}
      />
      <Field
        label="使ってよい道具"
        hint="選んだ道具だけを使います。選ばなければ道具なしで答えます。保存すると要る権限が自動で足されます"
        error={fieldErrors.get("allowed_tools")}
      >
        <div
          role="group"
          aria-label="使ってよい道具"
          className="space-y-0.5 rounded-lg border border-border p-1.5"
          data-testid="agent-tools"
        >
          {TOOLS.map((t) => {
            const checked = tools.includes(t.tool);
            const blocked = t.tool === "web.fetch" && fetchBlocked && !checked;
            return (
              <label key={t.tool} className={CHECK_ROW}>
                <input
                  type="checkbox"
                  checked={checked}
                  disabled={blocked}
                  onChange={(e) => toggleTool(t.tool, e.target.checked)}
                  className="mt-0.5 size-4 shrink-0 accent-[var(--primary)]"
                  aria-describedby={`agent-tool-${node.id}-${t.tool}`}
                />
                <span className="min-w-0 flex-1">
                  <span className="flex flex-wrap items-center gap-1.5">
                    <span className={cn(blocked && "text-muted-foreground")}>{t.label_ja}</span>
                    {t.reaches_outside ? (
                      <Badge variant="warning" className="px-1.5 py-0 text-[10px]">
                        社外へ出る
                      </Badge>
                    ) : null}
                  </span>
                  <span
                    id={`agent-tool-${node.id}-${t.tool}`}
                    className="block text-[11px] leading-snug text-muted-foreground"
                  >
                    {t.tool === "web.fetch" && blocked
                      ? settings.status === "loading"
                        ? "読んでよいサイトを確かめています…"
                        : "管理者が外部サイトを許可していないため選べません"
                      : `${t.description_ja}（${scopeLabel(t.required_scope)}）`}
                  </span>
                </span>
              </label>
            );
          })}
        </div>
      </Field>
      {fetchOn || hosts.length > 0 ? (
        <Field
          label="読んでよいサイト"
          hint={
            fetchOn
              ? "Web ページを読む道具は、ここで選んだサイトだけを読みます（管理者が許可したサイトから選ぶ）"
              : "Web ページを読む道具を選んでいないので使われません。外しておけます"
          }
          error={fieldErrors.get("egress_allowlist")}
        >
          {settings.status === "loading" ? (
            <div aria-busy="true" aria-label="読んでよいサイトを読み込み中">
              <Skeleton className="h-8 w-full" />
            </div>
          ) : hostOptions.length === 0 ? (
            <p className="text-[11px] text-destructive">
              {settings.status === "error"
                ? "許可されたサイトを読み込めませんでした。時間をおいて開き直してください"
                : "管理者が外部サイトを許可していません。この道具を外してください"}
            </p>
          ) : (
            <div role="group" aria-label="読んでよいサイト" className="space-y-0.5 rounded-lg border border-border p-1.5">
              {hostOptions.map((h) => {
                const outside = !ceiling.includes(h);
                return (
                  <label key={h} className={CHECK_ROW}>
                    <input
                      type="checkbox"
                      checked={hosts.includes(h)}
                      onChange={(e) => toggleHost(h, e.target.checked)}
                      className="mt-0.5 size-4 shrink-0 accent-[var(--primary)]"
                    />
                    <span className="min-w-0 flex-1 break-all font-mono text-xs">
                      {h}
                      {outside ? (
                        <span className="ml-1 font-sans text-[11px] text-destructive">（許可されたサイトの外）</span>
                      ) : null}
                    </span>
                  </label>
                );
              })}
            </div>
          )}
        </Field>
      ) : null}
      <ModelField
        value={typeof p.model === "string" ? p.model : undefined}
        onChange={(model) => patchParams(dispatch, node, { model })}
        error={fieldErrors.get("model")}
      />
      <div className="grid grid-cols-2 gap-3">
        <Field label="手順の上限（回）" hint="空欄で標準" error={fieldErrors.get("max_steps")}>
          <NumberInput
            value={typeof p.max_steps === "number" ? p.max_steps : null}
            min={1}
            max={settings.status === "ready" ? settings.data.max_steps_limit : 24}
            placeholder={settings.status === "ready" ? `標準 ${settings.data.default_max_steps}` : undefined}
            error={Boolean(fieldErrors.get("max_steps"))}
            onChange={(n) =>
              patchParams(dispatch, node, { max_steps: n === null ? undefined : Math.max(1, Math.round(n)) })
            }
          />
        </Field>
        <Field label="実行時間の上限（秒）" hint="空欄で標準" error={fieldErrors.get("max_duration_sec")}>
          <NumberInput
            value={typeof p.max_duration_sec === "number" ? p.max_duration_sec : null}
            min={settings.status === "ready" ? settings.data.min_duration_sec : 10}
            max={settings.status === "ready" ? settings.data.max_duration_sec : 3600}
            placeholder={settings.status === "ready" ? `標準 ${settings.data.default_max_duration_sec}` : undefined}
            error={Boolean(fieldErrors.get("max_duration_sec"))}
            onChange={(n) =>
              patchParams(dispatch, node, { max_duration_sec: n === null ? undefined : Math.round(n) })
            }
          />
        </Field>
      </div>
      <p className="text-[11px] leading-relaxed text-muted-foreground">
        結果は後ろのブロックから「前の結果」の回答（/text）として使えます。読める資料は実行する人の権限の範囲だけで、
        ファイルやデータへの書き込みはしません（書き込みは後ろのブロックで行います）。
      </p>
    </div>
  );
}
