"use client";

/// 使う人の権限の確かめ（GAP-37）の結果を、共有の画面と公開の確認に出す。
///
/// 共有した相手は、相手自身の権限でボタンを押し、表を見る。アプリやワークフローを共有しても、書き先のテーブルなどの
/// 権限が足りなければ使えないので、作る人に、部品が要る権限と、共有している相手ごとに足りない権限と直し方を見せる。
/// 判定はサーバー（`POST /mini-apps/{id}/access-check`・`POST /workflows/{id}/access-check`）が決め、ここは言葉にするだけ。
/// 相手ごとの結果は持ち主にだけ返る。自分に見えない対象は名前を出さず、相手の権限も確かめない。

import * as React from "react";
import { AlertCircle, CheckCircle2, Loader2, RotateCcw, TriangleAlert } from "lucide-react";

import { Button } from "@/components/ui/button";
import type { AccessReport, AccessStatus, SubjectAccess } from "@/lib/artifact-api";
import { fixHint, limitedNote, needLine, partPhrase } from "@/lib/access-wording";
import { cn } from "@/lib/utils";

/// 相手の呼び名（表示名が引けなければ種類で言う）。
function subjectName(s: SubjectAccess): string {
  if (s.display_name) return s.display_name;
  return s.subject.type === "role" ? `部署・グループ（${s.subject.id}）` : "名前を取得できないメンバー";
}

/// 相手の結果のうち、足りない・範囲が狭いもの（要る権限の添字つき）。
function problems(s: SubjectAccess): { index: number; status: AccessStatus }[] {
  return s.statuses.flatMap((status, index) =>
    status === "missing" || status === "limited" ? [{ index, status }] : [],
  );
}

export function AccessReportView({
  load,
  reloadKey = 0,
  mode,
  subject,
}: {
  /// 確かめを頼む（共有の画面は公開しているバージョン、公開の確認は公開するバージョン）。
  load: () => Promise<AccessReport>;
  /// 変わると確かめ直す（共有の相手を足した・外した）。
  reloadKey?: number;
  /// `share` は要る権限と相手ごとの直し方、`publish` は足りない相手だけを短く出す。
  mode: "share" | "publish";
  /// 確かめる物の呼び名（このアプリ・このワークフロー）。
  subject: "app" | "workflow";
}) {
  const [report, setReport] = React.useState<AccessReport | null>(null);
  const [failed, setFailed] = React.useState(false);
  const [attempt, setAttempt] = React.useState(0);
  const loadRef = React.useRef(load);
  loadRef.current = load;

  React.useEffect(() => {
    let live = true;
    setFailed(false);
    loadRef
      .current()
      .then((r) => live && setReport(r))
      .catch(() => {
        if (!live) return;
        setReport(null);
        setFailed(true);
      });
    return () => {
      live = false;
    };
  }, [reloadKey, attempt]);

  const title = mode === "share" ? "使う人に要る権限" : "共有している相手の権限";
  const thing = subject === "app" ? "アプリ" : "ワークフロー";

  if (failed) {
    return (
      <section aria-label={title} className="mt-4 flex flex-col items-start gap-2 text-sm" data-testid="access-report">
        <p className="flex items-start gap-2 text-destructive" role="alert">
          <AlertCircle className="mt-0.5 size-4 shrink-0" aria-hidden />
          使う人の権限を確かめられませんでした。
        </p>
        <Button type="button" size="sm" variant="outline" onClick={() => setAttempt((n) => n + 1)}>
          <RotateCcw className="size-4" aria-hidden />
          もう一度確かめる
        </Button>
      </section>
    );
  }
  if (!report) {
    return (
      <p className="mt-4 flex items-center gap-2 text-xs text-muted-foreground" role="status" data-testid="access-report-loading">
        <Loader2 className="size-3.5 animate-spin" aria-hidden />
        使う人の権限を確かめています…
      </p>
    );
  }

  const subjects = report.subjects ?? null;
  const withProblems = (subjects ?? []).filter((s) => problems(s).length > 0);
  const missingCount = (subjects ?? []).filter((s) => s.statuses.includes("missing")).length;
  // 確かめられなかったもの（実行のときに決まる・見えない・上限）があれば、「全員使える」と言い切らない（付録 C の P7）。
  const anyUnchecked = (subjects ?? []).some((s) => s.statuses.includes("unchecked"));
  const scope = anyUnchecked ? "確かめられた範囲では、" : "";
  const hasDynamic = report.needs.some((n) => n.dynamic);
  const hasHidden = report.needs.some((n) => !n.dynamic && !n.target);

  // 公開の確認: 相手ごとの結果があるとき（持ち主）だけ、足りない相手を短く出す。
  if (mode === "publish") {
    if (!subjects || subjects.length === 0) return null;
    return (
      <section aria-label={title} data-testid="access-report" className="space-y-1">
        <h3 className="text-xs font-medium text-foreground/70">{title}</h3>
        {missingCount === 0 ? (
          <p className="flex items-start gap-1.5 text-muted-foreground" data-testid="access-report-all-ok">
            <CheckCircle2 className="mt-0.5 size-4 shrink-0 text-primary" aria-hidden />
            {scope}共有している相手は全員、このバージョンのボタンと表を使えます。
          </p>
        ) : (
          <div
            role="note"
            className="space-y-1 rounded-lg border border-amber-500/30 bg-amber-500/5 px-3 py-2 text-xs text-amber-700 dark:text-amber-400"
          >
            <p className="font-medium">共有している相手のうち {missingCount} 件は、このバージョンの一部を使えません。</p>
            <ul className="list-disc space-y-0.5 pl-4" data-testid="access-report-missing">
              {subjects
                .filter((s) => s.statuses.includes("missing"))
                .map((s) => {
                  const first = s.statuses.indexOf("missing");
                  return (
                    <li key={`${s.subject.type}:${s.subject.id}`} className="[overflow-wrap:anywhere]">
                      {subjectName(s)}: {needLine(report.needs[first])}
                    </li>
                  );
                })}
            </ul>
            <p>公開はできます。相手ごとの直し方は、共有の画面で見られます。</p>
          </div>
        )}
      </section>
    );
  }

  return (
    <section aria-label={title} className="mt-5 flex min-w-0 flex-col gap-2" data-testid="access-report">
      <div>
        <h3 className="text-sm font-medium">{title}</h3>
        <p className="mt-0.5 text-xs text-muted-foreground">
          {subject === "app"
            ? "共有した相手は、相手自身の権限でボタンや表を使います。"
            : "共有した相手は、相手自身の権限でワークフローを実行します。"}
          足りない権限があれば、下の対象も共有してください。
        </p>
      </div>
      {report.needs.length === 0 ? (
        <p className="text-xs text-muted-foreground">この{thing}には、相手の権限が要る部品はありません。</p>
      ) : (
        <ul className="list-disc space-y-0.5 pl-5 text-xs" data-testid="access-report-needs">
          {report.needs.map((n, i) => (
            <li key={i} className="[overflow-wrap:anywhere]">
              {needLine(n)}
            </li>
          ))}
        </ul>
      )}
      {report.unavailable.length > 0 ? (
        <p className="flex items-start gap-1.5 text-xs text-amber-700 dark:text-amber-400" role="note">
          <TriangleAlert className="mt-0.5 size-3.5 shrink-0" aria-hidden />
          {report.unavailable.map((p) => partPhrase(p)).join("、")}が見つかりません。{thing}を編集して直してください。
        </p>
      ) : null}
      {subjects && subjects.length > 0 ? (
        withProblems.length === 0 ? (
          <p className="flex items-start gap-1.5 text-xs text-muted-foreground" data-testid="access-report-all-ok">
            <CheckCircle2 className="mt-0.5 size-3.5 shrink-0 text-primary" aria-hidden />
            {scope}共有している相手は全員、要る権限を持っています。
          </p>
        ) : (
          <ul className="flex flex-col gap-2" data-testid="access-report-subjects">
            {withProblems.map((s) => {
              const missing = s.statuses.includes("missing");
              return (
                <li
                  key={`${s.subject.type}:${s.subject.id}`}
                  data-testid="access-report-subject"
                  className={cn(
                    "rounded-lg border px-3 py-2 text-xs",
                    missing
                      ? "border-amber-500/30 bg-amber-500/5"
                      : "border-border bg-muted/40",
                  )}
                >
                  <p className="flex items-start gap-1.5 font-medium">
                    {missing ? (
                      <TriangleAlert className="mt-0.5 size-3.5 shrink-0 text-amber-600" aria-hidden />
                    ) : null}
                    <span className="min-w-0 [overflow-wrap:anywhere]">
                      {subjectName(s)}
                      {missing ? " は一部を使えません" : " は一部の範囲が狭くなります"}
                    </span>
                  </p>
                  <ul className="mt-1 space-y-1 pl-5">
                    {problems(s).map(({ index, status }) => {
                      const n = report.needs[index];
                      return (
                        <li key={index} className="list-disc [overflow-wrap:anywhere]">
                          {needLine(n)}
                          <span className="block text-muted-foreground">
                            {status === "missing" ? fixHint(n, s.subject.type) : limitedNote(n.need)}
                          </span>
                        </li>
                      );
                    })}
                  </ul>
                </li>
              );
            })}
          </ul>
        )
      ) : null}
      {hasDynamic || hasHidden || report.truncated || report.more_subjects ? (
        <ul className="space-y-0.5 text-[11px] text-muted-foreground" data-testid="access-report-notes">
          {hasDynamic ? <li>実行のときに決まる対象は、前もって確かめられません。</li> : null}
          {hasHidden ? (
            <li>あなたが見られないか、削除された対象は確かめていません。必要なら、対象の持ち主に共有を頼んでください。</li>
          ) : null}
          {report.truncated ? <li>対象が多いため、一部だけ確かめました。</li> : null}
          {report.more_subjects ? <li>共有している相手が多いため、最初の 20 件だけ確かめました。</li> : null}
        </ul>
      ) : null}
    </section>
  );
}
