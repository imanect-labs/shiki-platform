"use client";

/// スキルの編集（#919）。保存は新しいバージョンとして作る。

import { useParams } from "next/navigation";

import { SkillEditorPage } from "@/components/skills/skill-editor-page";

export default function EditSkillPage() {
  const { id } = useParams<{ id: string }>();
  return <SkillEditorPage key={id} target={{ kind: "edit", id }} />;
}
