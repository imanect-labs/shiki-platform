"use client";

/// dnd ワークフローエディタ（Task 10.12）。full-bleed（PageContainer 不使用）。
/// 読み込み中・エラーでも一覧への戻り口を出す（スマホはタイトルバーが無いため）。
///
/// 「AI に依頼」パネル（D-10 T10）: 文書のアシスタントと同じ形（PC は右に浮かぶカード・スマホは下からのシート）で、
/// 会話の対象をこのワークフローにする。開いている間は設定パネルを畳む（同時に 2 つの右パネルを出さない）。
/// AI が新しい版を保存したら、未保存の変更が無ければその場で読み直し、あれば帯で選ばせる（人の下書きを黙って
/// 捨てない）。残したまま保存すると、既存の expected_version の照合（409）が止めて読み直しを促す。

import * as React from "react";
import { useParams, useSearchParams } from "next/navigation";
import { Loader2, Sparkles, Workflow } from "lucide-react";

import { AssistantLayout } from "@/components/shell/assistant-sheet";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { toast } from "@/components/ui/use-toast";
import { usePageChrome } from "@/components/shell/page-header-context";
import { ConfigPanel } from "@/components/workflow/editor/config-panel";
import {
  WorkflowEditor,
  type EditorHandle,
  type EditorStatus,
} from "@/components/workflow/editor/workflow-editor";
import {
  WorkflowAssistantProvider,
  WorkflowChatPanel,
  type WorkflowAssistant,
} from "@/components/workflow/editor/workflow-assistant";
import { WorkflowChrome } from "@/components/workflow/header-actions";
import { workflowDraftStore } from "@/components/workflow/editor/draft-store";
import { useDocThreadDeepLink } from "@/hooks/use-doc-thread-deep-link";
import { getLayout, getWorkflow, type EditorLayout } from "@/lib/workflow-api";
import type { WorkflowIr } from "@/generated/workflow-ir";

type Loaded = { ir: WorkflowIr; version: number; layout: EditorLayout };

export default function WorkflowEditorPage() {
  const params = useParams<{ id: string }>();
  const id = params.id;
  const searchParams = useSearchParams();
  const [loaded, setLoaded] = React.useState<Loaded | null | "error">(null);
  // 読み直した回数（エディタを作り直すキー・AI が保存した版を読み込んだとき）。
  const [generation, setGeneration] = React.useState(0);
  const { chatOpen, setChatOpen, deepLinkThreadId } = useDocThreadDeepLink(searchParams.get("thread"));
  const [status, setStatus] = React.useState<EditorStatus | null>(null);
  const statusRef = React.useRef<EditorStatus | null>(null);
  const handleRef = React.useRef<EditorHandle | null>(null);
  // 未保存の変更があって、まだ読み込んでいない AI の版（帯を出す）。
  const [pendingAiVersion, setPendingAiVersion] = React.useState<number | null>(null);

  React.useEffect(() => {
    let cancelled = false;
    // 前に開いていたエディタの保存が続いていれば、終わってから読む（下書きの版を保存の結果に合わせてから）。
    workflowDraftStore
      .settled(id)
      .then(() => Promise.all([getWorkflow(id), getLayout(id)]))
      .then(([wf, layout]) => {
        if (!cancelled) setLoaded({ ir: wf.ir, version: wf.version, layout });
      })
      .catch(() => {
        if (!cancelled) setLoaded("error");
      });
    return () => {
      cancelled = true;
    };
  }, [id]);

  const onStatusChange = React.useCallback((s: EditorStatus) => {
    statusRef.current = s;
    setStatus(s);
    // 自分で保存して AI の版に追いついた（409 の後に読み直したなど）なら帯は要らない。
    setPendingAiVersion((v) => (v !== null && s.savedVersion >= v ? null : v));
  }, []);

  // 保存済みの最新を読み直し、エディタを作り直す（手元の下書きは捨てる）。
  const reload = React.useCallback(async (): Promise<number | null> => {
    workflowDraftStore.clear(id);
    try {
      const [wf, layout] = await Promise.all([getWorkflow(id), getLayout(id)]);
      setLoaded({ ir: wf.ir, version: wf.version, layout });
      setGeneration((g) => g + 1);
      setPendingAiVersion(null);
      return wf.version;
    } catch {
      toast({
        variant: "destructive",
        title: "新しい版を読み込めませんでした",
        description: "ページを再読み込みしてください。",
      });
      return null;
    }
  }, [id]);

  const onWorkflowSaved = React.useCallback(
    (ref: { id: string; version: number }) => {
      if (ref.id !== id) return;
      const s = statusRef.current;
      // 送り直し（再訪）で同じ版を受けても、読み込み済みなら何もしない。
      if (!s || ref.version <= s.savedVersion) return;
      if (s.dirty) {
        setPendingAiVersion((v) => Math.max(v ?? 0, ref.version));
        return;
      }
      void reload().then((version) => {
        if (version !== null) toast({ title: `AI が v${ref.version} を保存しました`, description: "最新の内容を読み込みました。" });
      });
    },
    [id, reload],
  );

  const assistant = React.useMemo<WorkflowAssistant>(
    () => ({ open: chatOpen, setOpen: setChatOpen }),
    [chatOpen, setChatOpen],
  );
  const save = React.useCallback(async () => (handleRef.current ? handleRef.current.save() : false), []);

  if (loaded === null || loaded === "error") return <PendingBody error={loaded === "error"} />;
  const name = loaded.ir.display_name || loaded.ir.name;
  return (
    <WorkflowAssistantProvider value={assistant}>
      <div className="flex h-full min-h-0 flex-col">
        <AssistantLayout
          open={chatOpen}
          onClose={() => setChatOpen(false)}
          label="ワークフローのアシスタント"
          title="AI に依頼"
          // 閉じても書きかけの依頼と会話の購読を残す（AI が保存した知らせを受け続ける）。
          keepMounted
          panel={
            <WorkflowChatPanel
              workflowId={id}
              name={name}
              initialThreadId={deepLinkThreadId}
              dirty={status?.dirty ?? false}
              savedVersion={status?.savedVersion ?? loaded.version}
              onSave={save}
              onWorkflowSaved={onWorkflowSaved}
            />
          }
        >
          <div className="flex min-h-0 min-w-0 flex-1 flex-col">
            {pendingAiVersion !== null ? (
              <AiVersionBanner
                version={pendingAiVersion}
                onLoad={() => void reload()}
                onKeep={() => setPendingAiVersion(null)}
              />
            ) : null}
            <div className="min-h-0 flex-1">
              <WorkflowEditor
                // id ごと・読み直しごとに reducer を作り直す（同一コンポーネント再利用で前の workflow の IR を
                // 新しい id に保存してしまう事故を防ぐ）。
                key={`${id}:${generation}`}
                workflowId={id}
                initialIr={loaded.ir}
                initialVersion={loaded.version}
                initialLayout={loaded.layout}
                // AI のパネルを開いている間は設定パネルを出さない（右のパネルを 2 つ重ねない）。
                renderSidePanel={chatOpen ? undefined : (ctx) => <ConfigPanel ctx={ctx} />}
                renderChrome={(ctx) => <WorkflowChrome ctx={ctx} />}
                onStatusChange={onStatusChange}
                handleRef={handleRef}
              />
            </div>
          </div>
        </AssistantLayout>
      </div>
    </WorkflowAssistantProvider>
  );
}

/// AI が新しい版を保存したが、手元に保存していない変更があって読み込んでいないときの帯。
function AiVersionBanner({
  version,
  onLoad,
  onKeep,
}: {
  version: number;
  onLoad: () => void;
  onKeep: () => void;
}) {
  return (
    <div
      role="status"
      className="flex shrink-0 flex-wrap items-center gap-x-3 gap-y-2 border-b bg-[color-mix(in_oklab,var(--season-autumn)_8%,transparent)] px-4 py-2 text-sm"
      data-testid="workflow-ai-version-banner"
    >
      <Sparkles className="size-4 shrink-0 text-[var(--season-autumn)]" aria-hidden />
      <span className="min-w-0 flex-1">
        AI が新しい版（v{version}）を保存しました。保存していない変更があるので、まだ読み込んでいません。
      </span>
      <span className="flex flex-wrap gap-2">
        <Button size="sm" variant="outline" onClick={onKeep}>
          自分の変更を残す
        </Button>
        <Button size="sm" onClick={onLoad}>
          AI の版を読み込む（自分の変更は捨てる）
        </Button>
      </span>
    </div>
  );
}

/// 読み込み中・開けないとき（エディタがクロームを登録する前の戻り口）。
function PendingBody({ error }: { error: boolean }) {
  usePageChrome(() => ({ back: { href: "/workflows", label: "ワークフロー一覧へ" } }), []);
  if (!error) {
    return (
      <div className="flex h-full items-center justify-center gap-2 text-sm text-muted-foreground">
        <Loader2 className="size-4 animate-spin" aria-hidden />
        読み込み中…
      </div>
    );
  }
  return (
    <div className="flex h-full items-center justify-center p-8">
      <EmptyState
        icon={Workflow}
        title="ワークフローを開けません"
        description="削除されたか、閲覧する権限がない可能性があります。"
      />
    </div>
  );
}
