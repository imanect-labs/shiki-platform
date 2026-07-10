"use client";

/// dnd ワークフローエディタ（Task 10.12）。エディタ本体は次 PR で実装（このページは骨組み）。

import * as React from "react";
import { Workflow } from "lucide-react";

import { EmptyState } from "@/components/ui/empty-state";

export default function WorkflowEditorPage() {
  return (
    <div className="flex h-full items-center justify-center p-8">
      <EmptyState
        icon={Workflow}
        title="エディタを準備中です"
        description="ドラッグ＆ドロップのワークフローエディタは次の更新で利用できます。"
      />
    </div>
  );
}
