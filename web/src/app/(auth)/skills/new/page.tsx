"use client";

/// スキルの新規作成（#919）。チャットの下書きカードからは `?thread=&name=` 付きで開く。

import { useSearchParams } from "next/navigation";
import * as React from "react";

import { SkillEditorPage } from "@/components/skills/skill-editor-page";

/// useSearchParams を使うため Suspense 境界でラップする（静的生成の CSR bailout 要件・Next.js）。
export default function NewSkillPage() {
  return (
    <React.Suspense fallback={<div className="h-full" />}>
      <NewSkillPageInner />
    </React.Suspense>
  );
}

function NewSkillPageInner() {
  const params = useSearchParams();
  const threadId = params.get("thread");
  const draftName = params.get("name");
  return (
    <SkillEditorPage
      target={{ kind: "new", threadId: threadId || null, draftName: draftName || null }}
    />
  );
}
