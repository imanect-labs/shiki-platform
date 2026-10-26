"use client";

/// 実行履歴の「AI エージェント」（agent.invoke）の結果（BUG-6）。
///
/// 回答（Markdown）・上限で打ち切ったかの目印・道具の使用の集計・使った資料・使えなかった道具を出す。
/// 回答は外から来た文章として描く（画像は読み込まずにリンクにし、外部リンクにはホスト名を添える・
/// 操作を持つ図は無効の文脈で描く）。形はカタログの `output_shape` と同じ（api の agent_port.rs が組む）。

import * as React from "react";
import { FileText, Globe } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { DataGridHostContext } from "@/components/data/data-grid-host";
import { Markdown } from "@/components/prompt-kit/markdown";
import { YuiActionContext } from "@/components/prompt-kit/yui-actions";
import { AGENT_TOOL_CATALOG } from "@/generated/workflow-catalog";

type Citation = { n?: number; file_id?: string; file_name?: string; url?: string; title?: string; snippet?: string };
type ToolCall = { tool: string; ok: boolean };

/// 出力の形（`agent.invoke` の output_shape）。読めない項目は無いものとして扱う。
type AgentOutput = {
  text?: unknown;
  complete?: unknown;
  stop?: unknown;
  citations?: unknown;
  tool_calls?: unknown;
  skipped_tools?: unknown;
  usage?: { steps?: unknown };
};

const STOP_LABELS: Record<string, string> = {
  truncated: "回答の長さの上限で途中まで",
  budget_steps: "手順の上限で打ち切り",
  budget_time: "実行時間の上限で打ち切り",
  budget_tokens: "使える量の上限で打ち切り",
  budget_cost: "費用の上限で打ち切り",
};

function toolLabel(tool: string): string {
  return AGENT_TOOL_CATALOG.find((t) => t.tool === tool)?.label_ja ?? tool;
}

/// 出力が AI エージェントの結果の形か（回答の本文を持つ）。
/// 出典の URL をリンクにしてよいか（http / https だけ。外部の検索サービスから来る値なので、ほかの scheme は文字で出す）。
function webLink(url: string): string | null {
  try {
    const u = new URL(url);
    return u.protocol === "http:" || u.protocol === "https:" ? u.href : null;
  } catch {
    return null;
  }
}

export function isAgentOutput(output: unknown): output is AgentOutput {
  return typeof output === "object" && output !== null && typeof (output as AgentOutput).text === "string";
}

export function AgentResult({ output }: { output: AgentOutput }) {
  const text = typeof output.text === "string" ? output.text : "";
  const stop = typeof output.stop === "string" ? output.stop : "completed";
  const citations = (Array.isArray(output.citations) ? output.citations : []) as Citation[];
  const skipped = (Array.isArray(output.skipped_tools) ? output.skipped_tools : []) as string[];
  const steps = typeof output.usage?.steps === "number" ? output.usage.steps : null;
  // 道具ごとの回数と失敗の数（呼んだ順を保つ）。
  const rawCalls = output.tool_calls;
  const counts = React.useMemo(() => {
    const calls = (Array.isArray(rawCalls) ? rawCalls : []) as ToolCall[];
    const m = new Map<string, { n: number; failed: number }>();
    for (const c of calls) {
      const e = m.get(c.tool) ?? { n: 0, failed: 0 };
      e.n += 1;
      if (!c.ok) e.failed += 1;
      m.set(c.tool, e);
    }
    return [...m.entries()];
  }, [rawCalls]);

  return (
    <div className="space-y-2" data-testid="agent-result">
      <div className="flex flex-wrap items-center gap-1.5">
        <p className="text-[11px] font-medium text-muted-foreground">エージェントの回答</p>
        {stop !== "completed" || output.complete === false ? (
          <Badge variant="warning" className="px-1.5 py-0 text-[10px]">
            {STOP_LABELS[stop] ?? "途中まで"}
          </Badge>
        ) : null}
        {steps !== null ? <span className="text-[10px] text-muted-foreground">{steps} 手順</span> : null}
      </div>
      <div className="max-h-80 overflow-auto rounded-md border bg-background p-2.5 scrollbar-subtle">
        <YuiActionContext.Provider value={null}>
          <DataGridHostContext.Provider value={null}>
            <Markdown untrusted className="text-[13px]">
              {text}
            </Markdown>
          </DataGridHostContext.Provider>
        </YuiActionContext.Provider>
      </div>
      {counts.length > 0 ? (
        <div className="flex flex-wrap items-center gap-1.5 text-[11px] text-muted-foreground">
          <span>使った道具:</span>
          {counts.map(([tool, c]) => (
            <Badge key={tool} variant={c.failed > 0 ? "destructive" : "muted"} className="px-1.5 py-0 text-[10px]">
              {toolLabel(tool)} ×{c.n}
              {c.failed > 0 ? `（失敗 ${c.failed}）` : ""}
            </Badge>
          ))}
        </div>
      ) : (
        <p className="text-[11px] text-muted-foreground">道具は使っていません</p>
      )}
      {citations.length > 0 ? (
        <div>
          <p className="mb-1 text-[11px] font-medium text-muted-foreground">使った資料</p>
          <ul className="space-y-1">
            {citations.map((c, i) => (
              <li key={c.file_id ?? c.url ?? i} className="flex min-w-0 items-start gap-1.5 text-[11px]">
                {c.url ? (
                  <Globe className="mt-0.5 size-3 shrink-0 text-muted-foreground" aria-hidden />
                ) : (
                  <FileText className="mt-0.5 size-3 shrink-0 text-muted-foreground" aria-hidden />
                )}
                <span className="min-w-0 break-all">
                  {c.url && webLink(c.url) ? (
                    <a
                      href={webLink(c.url) ?? undefined}
                      target="_blank"
                      rel="noopener noreferrer"
                      className="text-primary underline decoration-primary/30 underline-offset-2"
                    >
                      {c.title || c.url}
                    </a>
                  ) : (
                    <span>{c.title || c.url || c.file_name || c.file_id}</span>
                  )}
                  {c.snippet ? <span className="block text-muted-foreground">{c.snippet}</span> : null}
                </span>
              </li>
            ))}
          </ul>
        </div>
      ) : null}
      {skipped.length > 0 ? (
        <p className="text-[11px] text-muted-foreground">
          使えなかった道具: {skipped.map(toolLabel).join("、")}（権限・管理者の設定・この環境で使えないもの）
        </p>
      ) : null}
    </div>
  );
}
