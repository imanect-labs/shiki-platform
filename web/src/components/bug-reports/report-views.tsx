"use client";

import Link from "next/link";
import { useCallback, useEffect, useRef, useState } from "react";
import { useRouter } from "next/navigation";
import type { components } from "@/generated/api";
import { apiFetch } from "@/lib/api";
import { Button } from "@/components/ui/button";
import { toast } from "@/components/ui/use-toast";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
} from "@/components/ui/dialog";

type Schema = components["schemas"];
type Report = Schema["BugReport"];
type Policy = Schema["BugReportPolicy"];
type Detail = Schema["ReportDetail"];
type Content = Schema["ReportContent"];
const statuses: Record<Report["status"], string> = {
  draft: "下書き",
  pending_approval: "管理者の確認待ち",
  held: "保留（外部には未送信）",
  queued: "送信待ち",
  sending: "送信中",
  sent: "送信先で受付済み",
  rejected: "差し戻し",
  cancelled: "取り下げ済み",
  failed: "送信失敗",
  delivery_unknown: "送信結果を確認中",
};
class RequestError extends Error {
  constructor(public status: number) {
    super(
      status === 409
        ? "別の画面で内容が更新されました。最新の内容を確認してから操作してください。"
        : status === 400
          ? "入力内容を確認してください。URL・HTML・パスワードやAPIキーは記入できません。"
          : status === 403
            ? "この操作を行う権限がありません。"
            : status === 429
              ? "報告は1時間に5件、1日に20件までです。時間をおいてお試しください。"
              : "読み込み・保存ができませんでした。時間をおいて、もう一度お試しください。",
    );
  }
}
async function request<T>(
  path: string,
  method = "GET",
  body?: unknown,
): Promise<T> {
  const res = await apiFetch(path, {
    method,
    headers: {
      "Content-Type": "application/json",
      ...(method === "POST" ? { "Idempotency-Key": crypto.randomUUID() } : {}),
    },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  });
  if (!res.ok) throw new RequestError(res.status);
  return res.json() as Promise<T>;
}
function message(e: unknown) {
  return e instanceof Error
    ? e.message
    : "通信できませんでした。もう一度お試しください。";
}
function Frame({
  children,
  admin = false,
}: {
  children: React.ReactNode;
  admin?: boolean;
}) {
  return (
    <main className="mx-auto w-full max-w-3xl space-y-6 p-4 md:p-8">
      <header className="flex flex-wrap items-center justify-between gap-3">
        <h1 className="text-xl font-semibold">不具合報告</h1>
        <nav className="flex gap-4 text-sm">
          <Link href={admin ? "/admin/bug-reports" : "/bug-reports"}>
            報告一覧
          </Link>
          {admin ? (
            <Link href="/admin/bug-reports/settings">受付設定</Link>
          ) : (
            <Link href="/bug-reports/new">新しく報告する</Link>
          )}
        </nav>
      </header>
      {children}
    </main>
  );
}
function ErrorNotice({ error, retry }: { error: string; retry?: () => void }) {
  return error ? (
    <div
      role="alert"
      className="space-y-2 rounded-lg border border-destructive/40 bg-destructive/5 p-4 text-sm"
    >
      <p>{error}</p>
      {retry ? (
        <Button variant="outline" onClick={retry}>
          もう一度読み込む
        </Button>
      ) : null}
    </div>
  ) : null;
}
function useRefresh(load: () => Promise<void>) {
  useEffect(() => {
    void load();
    const focus = () => {
      void load();
    };
    window.addEventListener("focus", focus);
    const timer = window.setInterval(focus, 30000);
    return () => {
      window.removeEventListener("focus", focus);
      clearInterval(timer);
    };
  }, [load]);
}
export function ReportList({ admin = false }: { admin?: boolean }) {
  const [data, setData] = useState<Schema["ReportPage"] | null>(null);
  const [error, setError] = useState("");
  // 表示済みの件数。自動更新では、この件数に届くまでページをたどって状態を取り直す。
  const loaded = useRef(0);
  const [loadingMore, setLoadingMore] = useState(false);
  const load = useCallback(async () => {
    const base = admin ? "/org-admin/bug-reports" : "/bug-reports";
    try {
      let page = await request<Schema["ReportPage"]>(base);
      const items = [...page.items];
      const ids = new Set(items.map((item) => item.id));
      // 追加表示した範囲も最新にする（上限 20 ページ）。
      for (
        let pages = 1;
        items.length < loaded.current && page.next_cursor && pages < 20;
        pages++
      ) {
        page = await request<Schema["ReportPage"]>(
          `${base}?cursor=${page.next_cursor}`,
        );
        for (const item of page.items) {
          if (!ids.has(item.id)) {
            ids.add(item.id);
            items.push(item);
          }
        }
      }
      const truncated = items.length < loaded.current && page.next_cursor;
      setData((previous) => {
        if (!truncated || !previous) {
          loaded.current = items.length;
          return { items, next_cursor: page.next_cursor };
        }
        // 上限で取り切れなかった表示済みの範囲は、前回の値とカーソルのまま残す。
        const merged = [
          ...items,
          ...previous.items.filter((item) => !ids.has(item.id)),
        ];
        loaded.current = merged.length;
        return { items: merged, next_cursor: previous.next_cursor };
      });
      setError("");
    } catch (e) {
      setData(null);
      setError(message(e));
    }
  }, [admin]);
  useRefresh(load);
  return (
    <Frame admin={admin}>
      <p className="text-sm text-muted-foreground">
        {admin
          ? "利用者から提出された報告を確認します。会話の内容や他の人の下書きは表示しません。"
          : "自分の報告の状況を確認できます。「受付済み」は、不具合が直ったことを表すものではありません。"}
      </p>
      <ErrorNotice error={error} retry={() => void load()} />
      {!data && !error ? <p role="status">読み込み中…</p> : null}
      {data?.items.length === 0 ? (
        <div className="rounded-xl border border-border/60 bg-card/40 p-8 text-center">
          <p>報告はまだありません</p>
          {!admin ? (
            <Button asChild className="mt-4">
              <Link href="/bug-reports/new">新しく報告する</Link>
            </Button>
          ) : null}
        </div>
      ) : null}
      {admin && data ? (
        <p className="text-sm">
          表示中の確認待ち：
          {data.items.filter((r) => r.status === "pending_approval").length}件
        </p>
      ) : null}
      <ul className="divide-y rounded-xl border border-border/60 bg-card/40">
        {data?.items.map((item) => (
          <li key={item.id}>
            <Link
              className="flex flex-col gap-2 p-4 hover:bg-muted/40 focus-visible:ring-2 focus-visible:ring-ring"
              href={`${admin ? "/admin" : ""}/bug-reports/${item.id}`}
            >
              <span className="break-words font-medium">{item.title}</span>
              <span className="text-sm text-muted-foreground">
                {statuses[item.status]}
              </span>
            </Link>
          </li>
        ))}
      </ul>
      {data?.next_cursor ? (
        <Button
          variant="outline"
          disabled={loadingMore}
          onClick={async () => {
            setLoadingMore(true);
            try {
              const next = await request<Schema["ReportPage"]>(
                `${admin ? "/org-admin" : ""}/bug-reports?cursor=${data.next_cursor}`,
              );
              setData((previous) => {
                const items = previous?.items ?? [];
                const ids = new Set(items.map((item) => item.id));
                const merged = [
                  ...items,
                  ...next.items.filter((item) => !ids.has(item.id)),
                ];
                loaded.current = merged.length;
                return { items: merged, next_cursor: next.next_cursor };
              });
            } catch (e) {
              setError(message(e));
            } finally {
              setLoadingMore(false);
            }
          }}
        >
          続きを表示
        </Button>
      ) : null}
    </Frame>
  );
}
const empty: Content = {
  category: "bug",
  title: "",
  symptom: "",
  steps_tried: "",
  expected_behavior: "",
};
function ContentFields({
  content,
  setContent,
}: {
  content: Content;
  setContent: (c: Content) => void;
}) {
  return (
    <>
      <label className="block space-y-2">
        <span>種別</span>
        <select
          className="block w-full rounded-md border bg-background p-2"
          value={content.category}
          onChange={(e) =>
            setContent({
              ...content,
              category: e.target.value as Content["category"],
            })
          }
        >
          <option value="bug">うまく動かない</option>
          <option value="usage">使い方で困っている</option>
          <option value="other">その他</option>
        </select>
      </label>
      {(
        [
          ["title", "題名", 200],
          ["symptom", "どのようなことが起きましたか", 2000],
          ["steps_tried", "試したこと", 2000],
          ["expected_behavior", "本来どう動いてほしいですか", 2000],
        ] as const
      ).map(([key, label, max]) => (
        <label key={key} className="block space-y-2">
          <span>
            {label}
            {key === "title" ? "（必須）" : ""}
          </span>
          {key === "title" ? (
            <Input
              aria-label="題名（必須）"
              required
              maxLength={max}
              value={content[key]}
              onChange={(e) =>
                setContent({ ...content, [key]: e.target.value })
              }
            />
          ) : (
            <Textarea
              aria-label={label}
              rows={4}
              maxLength={max}
              value={content[key]}
              onChange={(e) =>
                setContent({ ...content, [key]: e.target.value })
              }
            />
          )}
          <span className="text-xs text-muted-foreground">
            {content[key].length} / {max}文字
          </span>
        </label>
      ))}
    </>
  );
}
function ContentPreview({ content }: { content: Content }) {
  return (
    <dl className="space-y-3 rounded-xl border border-border/60 bg-card/40 p-4">
      <div>
        <dt className="text-sm text-muted-foreground">種別</dt>
        <dd>
          {content.category === "bug"
            ? "うまく動かない"
            : content.category === "usage"
              ? "使い方で困っている"
              : "その他"}
        </dd>
      </div>
      {(
        [
          ["title", "題名"],
          ["symptom", "症状"],
          ["steps_tried", "試したこと"],
          ["expected_behavior", "期待する動作"],
        ] as const
      ).map(([key, label]) => (
        <div key={key}>
          <dt className="text-sm text-muted-foreground">{label}</dt>
          <dd className="whitespace-pre-wrap break-words">
            {content[key] || "記入なし"}
          </dd>
        </div>
      ))}
    </dl>
  );
}
export function NewReport({
  reportId,
  refreshError = "",
  retryRefresh,
}: {
  reportId?: string;
  refreshError?: string;
  retryRefresh?: () => void;
} = {}) {
  const router = useRouter();
  const [policy, setPolicy] = useState<Schema["ReporterPolicy"] | null>(null);
  const [content, setContent] = useState<Content>(empty);
  const [consent, setConsent] = useState(false);
  const [confirm, setConfirm] = useState(false);
  const [draft, setDraft] = useState<Report | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const reload = useCallback(async (id?: string) => {
    const [latestPolicy, detail] = await Promise.all([
      request<Schema["ReporterPolicy"]>("/bug-reports/policy"),
      id ? request<Detail>(`/bug-reports/${id}`) : Promise.resolve(null),
    ]);
    setPolicy(latestPolicy);
    if (detail) {
      setDraft(detail.report);
      setContent(detail.report.content);
      setConsent(false);
    }
  }, []);
  const load = useCallback(async () => {
    try {
      await reload(reportId);
    } catch (e) {
      setError(message(e));
    }
  }, [reload, reportId]);
  useEffect(() => {
    void load();
  }, [load]);
  async function saveOrCancel(cancel = false) {
    if (!draft || !policy || busy) return;
    setBusy(true);
    setError("");
    try {
      const updated = await request<Report>(
        `/bug-reports/${draft.id}${cancel ? "/cancel" : ""}`,
        cancel ? "POST" : "PATCH",
        cancel
          ? { expected_rev: draft.rev }
          : {
              expected_rev: draft.rev,
              content: policy.policy.allow_content_sharing
                ? content
                : {
                    ...empty,
                    category: content.category,
                    title: "不具合報告（カテゴリのみ）",
                  },
            },
      );
      setDraft(updated);
      if (cancel) router.push("/bug-reports");
      else toast({ title: "下書きを保存しました" });
    } catch (e) {
      setError(message(e));
      if (e instanceof RequestError && e.status === 409) {
        try {
          await reload(draft.id);
        } catch (reloadError) {
          setError(message(reloadError));
        }
      }
    } finally {
      setBusy(false);
    }
  }
  async function submit() {
    if (!policy || busy) return;
    setBusy(true);
    setError("");
    let currentId = draft?.id;
    try {
      let current =
        draft ??
        (await request<Report>(
          "/bug-reports",
          "POST",
          policy.policy.allow_content_sharing
            ? content
            : {
                ...empty,
                category: content.category,
                title: "不具合報告（カテゴリのみ）",
              },
        ));
      if (draft)
        current = await request<Report>(`/bug-reports/${draft.id}`, "PATCH", {
          expected_rev: draft.rev,
          content: policy.policy.allow_content_sharing
            ? content
            : {
                ...empty,
                category: content.category,
                title: "不具合報告（カテゴリのみ）",
              },
        });
      setDraft(current);
      currentId = current.id;
      await request(`/bug-reports/${current.id}/submit`, "POST", {
        expected_rev: current.rev,
        policy_rev: policy.policy.rev,
        destination_version: policy.destination_version,
        consent: policy.policy.allow_content_sharing && consent,
        notice_version: "bug-report-v1",
      });
      toast({
        title: "報告を受け付けました",
        description: policy.destination_available
          ? "報告一覧で状況を確認できます。"
          : "送信先が未設定のため保留しました。外部には送信していません。",
      });
      router.push(`/bug-reports/${current.id}?submitted=1`);
    } catch (e) {
      setError(message(e));
      if (e instanceof RequestError && e.status === 409) {
        try {
          await reload(currentId);
          setError(
            "別の画面で内容が更新されました。最新の内容を読み直しました。確認してから操作してください。",
          );
        } catch (reloadError) {
          setError(
            `最新の内容を読み込めませんでした。もう一度読み込んでください。${message(reloadError)}`,
          );
        }
      }
      setConfirm(false);
    } finally {
      setBusy(false);
    }
  }
  return (
    <Frame>
      <h2 className="text-lg font-medium">
        {reportId ? "下書きを編集する" : "新しく報告する"}
      </h2>
      <ErrorNotice error={refreshError} retry={retryRefresh} />
      <ErrorNotice error={error} retry={() => void load()} />
      {!policy ? (
        <p role="status">設定を読み込み中…</p>
      ) : !policy.policy.enabled ? (
        <p>現在、報告の受付は停止しています。</p>
      ) : (
        <form
          className="space-y-5"
          onSubmit={(e) => {
            e.preventDefault();
            setConfirm(true);
          }}
        >
          <p className="text-sm text-muted-foreground">
            個人情報・社内文書・パスワードは書かないでください。URL・画像・添付・会話のリンクは送れません。
          </p>
          {policy.policy.allow_content_sharing ? (
            <>
              <ContentFields
                content={content}
                setContent={(c) => {
                  setContent(c);
                }}
              />
              <label className="flex gap-3">
                <input
                  type="checkbox"
                  checked={consent}
                  onChange={(e) => setConsent(e.target.checked)}
                />
                <span>
                  記入した本文を組織の管理者と送信先に共有することに同意します。同意しない場合は種別だけを報告します。
                </span>
              </label>
            </>
          ) : (
            <>
              <p>
                組織の設定で本文の共有が禁止されています。種別だけを報告できます。
              </p>
              <label className="block">
                種別
                <select
                  className="ml-3 rounded border bg-background p-2"
                  value={content.category}
                  onChange={(e) =>
                    setContent({
                      ...content,
                      category: e.target.value as Content["category"],
                    })
                  }
                >
                  <option value="bug">うまく動かない</option>
                  <option value="usage">使い方で困っている</option>
                  <option value="other">その他</option>
                </select>
              </label>
            </>
          )}
          <p className="text-sm">
            {policy.destination_available
              ? "送信先：GitHub の imanect-labs/shiki-test（不具合受付）。外部で保持される内容は、この画面で取り下げても削除されません。"
              : "送信先は未設定です。組織内で受け付けて保留し、外部には送信しません。設定後も、改めて本人の確認が必要です。"}
          </p>
          <p className="text-sm">
            {policy.policy.require_approval
              ? "送信には管理者の承認が必要です。"
              : "管理者の承認なしで送信します。"}
          </p>
          <div className="flex flex-wrap gap-3">
            <Button type="submit" disabled={busy}>
              送信内容を確認
            </Button>
            {draft ? (
              <Button
                type="button"
                variant="outline"
                disabled={busy}
                onClick={() => void saveOrCancel()}
              >
                下書きを保存
              </Button>
            ) : null}
          </div>
        </form>
      )}
      {draft ? (
        <Button
          variant="outline"
          disabled={busy}
          onClick={() => void saveOrCancel(true)}
        >
          報告を取り下げる
        </Button>
      ) : null}
      <Dialog open={confirm} onOpenChange={setConfirm}>
        <DialogContent className="max-h-[85dvh] overflow-y-auto">
          <DialogHeader>
            <DialogTitle>この内容で報告しますか</DialogTitle>
            <DialogDescription>
              会話や添付は送りません。送信した内容を後から編集することはできません。
            </DialogDescription>
          </DialogHeader>
          {policy?.policy.allow_content_sharing && consent ? (
            <ContentPreview content={content} />
          ) : (
            <p>
              種別のみ：
              {content.category === "bug"
                ? "うまく動かない"
                : content.category === "usage"
                  ? "使い方で困っている"
                  : "その他"}
              。本文は共有しません。
            </p>
          )}
          <DialogFooter>
            <Button
              variant="outline"
              disabled={busy}
              onClick={() => setConfirm(false)}
            >
              戻る
            </Button>
            <Button disabled={busy} onClick={() => void submit()}>
              {busy ? "受付中…" : "この内容で報告する"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </Frame>
  );
}
export function ReportDetailView({
  id,
  admin = false,
}: {
  id: string;
  admin?: boolean;
}) {
  const [detail, setDetail] = useState<Detail | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [reason, setReason] = useState("");
  const load = useCallback(async () => {
    try {
      const d = await request<Detail>(
        `${admin ? "/org-admin" : ""}/bug-reports/${id}`,
      );
      // 下書きフォームは独自の編集状態を保持する。自動更新で再初期化しない。
      setDetail((previous) =>
        !admin &&
        previous?.report.status === "draft" &&
        d.report.status === "draft"
          ? previous
          : d,
      );
      setError("");
    } catch (e) {
      if (e instanceof RequestError && [403, 404].includes(e.status))
        setDetail(null);
      setError(message(e));
    }
  }, [id, admin]);
  useRefresh(load);
  async function act(operation: string) {
    if (!detail || busy) return;
    setBusy(true);
    setError("");
    try {
      await request(
        `${admin ? "/org-admin" : ""}/bug-reports/${id}/${operation}`,
        "POST",
        { expected_rev: detail.report.rev, reason },
      );
      await load();
    } catch (e) {
      setError(message(e));
      if (e instanceof RequestError && e.status === 409) await load();
    } finally {
      setBusy(false);
    }
  }
  if (detail && !admin && detail.report.status === "draft")
    return (
      <NewReport
        key={id}
        reportId={id}
        refreshError={error}
        retryRefresh={() => void load()}
      />
    );
  return (
    <Frame admin={admin}>
      <ErrorNotice error={error} retry={() => void load()} />
      {!detail && !error ? <p role="status">読み込み中…</p> : null}
      {detail ? (
        <>
          <h2 className="text-lg font-medium">{detail.report.content.title}</h2>
          <p role="status">{statuses[detail.report.status]}</p>
          {detail.report.status === "held" ? (
            <p>
              外部には送信していません。送り先の設定後、本人による再確認が必要です。
            </p>
          ) : null}
          <ContentPreview
            content={detail.submission?.content ?? detail.report.content}
          />
          {detail.submission ? (
            <div className="space-y-2 text-sm">
              <p>
                本文共有：
                {detail.submission.content_consent
                  ? "同意あり"
                  : "本文は共有していません"}
              </p>
              <p>
                送信先：
                {detail.submission.sink_profile_id === "disabled"
                  ? "未設定"
                  : "GitHub: imanect-labs/shiki-test"}
                （設定版 {detail.submission.sink_config_version}）
              </p>
              {admin ? (
                <p>
                  報告者：{detail.report.reporter_id} ／ 承認者：
                  {detail.submission.approved_by ?? "未承認"}
                </p>
              ) : null}
              {detail.submission.consent_at ? (
                <p>
                  本人の確認日時：
                  {new Date(detail.submission.consent_at).toLocaleString(
                    "ja-JP",
                  )}
                </p>
              ) : null}
              {detail.submission.approved_at ? (
                <p>
                  承認日時：
                  {new Date(detail.submission.approved_at).toLocaleString(
                    "ja-JP",
                  )}
                </p>
              ) : null}
              {detail.submission.rejected_at ? (
                <p>
                  差し戻し日時：
                  {new Date(detail.submission.rejected_at).toLocaleString(
                    "ja-JP",
                  )}
                </p>
              ) : null}
              {detail.submission.rejection_reason ? (
                <p>
                  管理者からの差し戻し理由：{detail.submission.rejection_reason}
                </p>
              ) : null}
            </div>
          ) : null}
          {admin && detail.report.status === "pending_approval" ? (
            <div className="space-y-3">
              <Button disabled={busy} onClick={() => void act("approve")}>
                承認する
              </Button>
              <label className="block space-y-2">
                <span>差し戻しの理由（報告した本人にも表示されます）</span>
                <Textarea
                  value={reason}
                  maxLength={2000}
                  onChange={(e) => setReason(e.target.value)}
                />
              </label>
              <Button
                variant="outline"
                disabled={busy || !reason.trim()}
                onClick={() => void act("reject")}
              >
                差し戻す
              </Button>
            </div>
          ) : null}
          {admin && detail.report.status === "failed" ? (
            <Button disabled={busy} onClick={() => void act("retry")}>
              安全に再送できるか確認して再送
            </Button>
          ) : null}
          {!admin &&
          ["draft", "pending_approval", "queued", "held"].includes(
            detail.report.status,
          ) ? (
            <Button
              variant="outline"
              disabled={busy}
              onClick={() => void act("cancel")}
            >
              報告を取り下げる
            </Button>
          ) : null}
          {!admin && ["held", "rejected"].includes(detail.report.status) ? (
            <Button disabled={busy} onClick={() => void act("reopen")}>
              下書きに戻す
            </Button>
          ) : null}
        </>
      ) : null}
    </Frame>
  );
}
export function ReportSettings() {
  const [policy, setPolicy] = useState<Policy | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [saved, setSaved] = useState(false);
  const load = useCallback(async () => {
    try {
      setPolicy(await request("/org-admin/bug-reports/settings"));
    } catch (e) {
      setPolicy(null);
      setError(message(e));
    }
  }, []);
  useEffect(() => {
    void load();
  }, [load]);
  return (
    <Frame admin>
      <h2 className="text-lg font-medium">受付設定</h2>
      <ErrorNotice error={error} retry={() => void load()} />
      {!policy && !error ? <p role="status">読み込み中…</p> : null}
      {saved ? <p role="status">設定を保存しました。</p> : null}
      {policy ? (
        <form
          className="space-y-6 rounded-xl border border-border/60 bg-card/40 p-5"
          onSubmit={async (e) => {
            e.preventDefault();
            setBusy(true);
            setSaved(false);
            setError("");
            try {
              setPolicy(
                await request("/org-admin/bug-reports/settings", "PUT", policy),
              );
              setSaved(true);
            } catch (e) {
              setError(message(e));
              if (e instanceof RequestError && e.status === 409) await load();
            } finally {
              setBusy(false);
            }
          }}
        >
          {(
            [
              ["enabled", "報告を受け付ける"],
              ["require_approval", "送信前に管理者の承認を必須にする"],
              ["allow_content_sharing", "利用者が記入した本文の共有を許可する"],
            ] as const
          ).map(([key, label]) => (
            <label key={key} className="flex gap-3">
              <input
                type="checkbox"
                checked={policy[key]}
                onChange={(e) =>
                  setPolicy({ ...policy, [key]: e.target.checked })
                }
              />
              <span>{label}</span>
            </label>
          ))}
          <p className="text-sm text-muted-foreground">
            本文の共有には、報告する本人の同意も必要です。承認を不要に変更しても、確認待ちの報告は自動送信されません。送信先と接続情報は運用者が設定します。
          </p>
          <Button type="submit" disabled={busy}>
            {busy ? "保存中…" : "設定を保存"}
          </Button>
        </form>
      ) : null}
    </Frame>
  );
}
