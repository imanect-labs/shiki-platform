"use client";
import { createContext, useCallback, useContext, useEffect, useState } from "react";
import Link from "next/link";
import type { components } from "@/generated/api";
import { apiFetch } from "@/lib/api";
import { Button } from "@/components/ui/button";

type Case = components["schemas"]["HelpCase"];
export const BugReportActionContext = createContext<(() => void) | null>(null);

export function HelpResolutionCard({ threadId, caseId }: { threadId: string; caseId: string }) {
  const createDraft = useContext(BugReportActionContext);
  const [value, setValue] = useState<Case | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [privateOnly, setPrivateOnly] = useState(false);
  const path = `/threads/${threadId}/help-cases/${caseId}`;
  const load = useCallback(async () => {
    const res = await apiFetch(path);
    if (res.status === 403 || res.status === 404) { setPrivateOnly(true); return; }
    if (!res.ok) throw new Error("確認状態を読み込めませんでした。");
    setValue(await res.json() as Case); setPrivateOnly(false);
  }, [path]);
  useEffect(() => {
    void load().catch((e: Error) => setError(e.message));
    const refresh = () => { void load().catch(() => {}); };
    window.addEventListener("focus", refresh);
    return () => window.removeEventListener("focus", refresh);
  }, [load]);
  async function act(operation: string, body?: string) {
    setBusy(true); setError("");
    try {
      const res = await apiFetch(`${path}/${operation}`, { method: "POST", headers: { "Content-Type": "application/json" }, ...(body ? { body: JSON.stringify(body) } : {}) });
      if (!res.ok) {
        await load();
        throw new Error(res.status === 409 ? "確認状態が変わりました。最新の状態を確認してください。" : "操作できませんでした。案内と閲覧権限を確認してください。");
      }
      setValue(await res.json() as Case);
      if (operation === "report-intent") createDraft?.();
    } catch (e) { setError(e instanceof Error ? e.message : "通信できませんでした。"); }
    finally { setBusy(false); }
  }
  if (privateOnly) return <p className="text-xs text-muted-foreground">相談した本人用の確認カードです。</p>;
  return <section className="my-3 space-y-3 rounded-xl border p-4" aria-label="ヘルプの解決確認" aria-busy={busy}>
    {error && <p role="alert" className="text-sm text-destructive">{error} <button className="underline" onClick={() => void load().catch((e: Error) => setError(e.message))}>再読み込み</button></p>}
    {!value ? <p role="status">確認状態を読み込み中…</p> : value.report_id ? <Link href={`/bug-reports/${value.report_id}`} className="text-sm underline">報告の要約を確認・編集する</Link> : value.resolution === "resolved" ? <p className="text-sm">解決済みです。</p> : value.report_intent ? <p className="text-sm">報告の下書きを準備します。要約が届いたら、内容を確認して送信してください。</p> : value.hit_count > 0 && value.resolution !== "unresolved" ? <>
      <p className="text-sm font-medium">この手順で解決しましたか？</p>
      <div className="flex flex-wrap gap-2"><Button size="sm" variant="outline" disabled={busy} onClick={() => void act("resolution", "resolved")}>解決した</Button><Button size="sm" variant="outline" disabled={busy} onClick={() => void act("resolution", "unresolved")}>解決しなかった</Button></div>
    </> : <>
      <p className="text-sm font-medium">不具合として報告しますか？</p>
      <p className="text-xs text-muted-foreground">AI が症状・試した手順・期待する動作を要約します。送信前にご自身で確認できます。</p>
      <Button size="sm" disabled={busy || !createDraft} onClick={() => void act("report-intent")}>下書きを作る</Button>
    </>}
  </section>;
}
export function BugReportDraftCard({ reportId }: { reportId: string }) {
  const [allowed, setAllowed] = useState<boolean | null>(null);
  useEffect(() => { let active = true; void apiFetch(`/bug-reports/${reportId}`).then((r) => { if (active) setAllowed(r.ok); }).catch(() => { if (active) setAllowed(false); }); return () => { active = false; }; }, [reportId]);
  if (allowed === null) return <p role="status" className="text-sm">下書きを確認中…</p>;
  if (!allowed) return <p className="text-xs text-muted-foreground">本人用の報告下書きです。</p>;
  return <section className="my-3 space-y-2 rounded-xl border p-4" aria-label="不具合報告の下書き"><p className="text-sm font-medium">不具合報告の下書きができました</p><p className="text-xs text-muted-foreground">まだ送信していません。要約・送信先・共有への同意を確認してください。</p><Link className="text-sm underline" href={`/bug-reports/${reportId}`}>要約を確認・編集する</Link></section>;
}
