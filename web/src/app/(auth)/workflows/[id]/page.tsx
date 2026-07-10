"use client";

/// dnd ワークフローエディタ（Task 10.12）。full-bleed（PageContainer 不使用）。

import * as React from "react";
import { useParams } from "next/navigation";
import { Loader2, Workflow } from "lucide-react";

import { EmptyState } from "@/components/ui/empty-state";
import { WorkflowEditor } from "@/components/workflow/editor/workflow-editor";
import { getLayout, getWorkflow, type EditorLayout } from "@/lib/workflow-api";
import type { WorkflowIr } from "@/generated/workflow-ir";

type Loaded = { ir: WorkflowIr; version: number; layout: EditorLayout };

export default function WorkflowEditorPage() {
  const params = useParams<{ id: string }>();
  const id = params.id;
  const [loaded, setLoaded] = React.useState<Loaded | null | "error">(null);

  React.useEffect(() => {
    let cancelled = false;
    Promise.all([getWorkflow(id), getLayout(id)])
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

  if (loaded === null) {
    return (
      <div className="flex h-full items-center justify-center gap-2 text-sm text-muted-foreground">
        <Loader2 className="size-4 animate-spin" aria-hidden />
        読み込み中…
      </div>
    );
  }
  if (loaded === "error") {
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
  return (
    <WorkflowEditor
      // id ごとに reducer を作り直す（同一コンポーネント再利用で前の workflow の IR を
      // 新しい id に保存してしまう事故を防ぐ）。
      key={id}
      workflowId={id}
      initialIr={loaded.ir}
      initialVersion={loaded.version}
      initialLayout={loaded.layout}
    />
  );
}
