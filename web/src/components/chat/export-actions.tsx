"use client";

/// 書き出しの操作（ノートとして開く・Word としてドライブに保存・md／docx のダウンロード）。回答 1 件の
/// メニュー（[`MessageExportMenu`]）と会話全体のダイアログ（[`ThreadExportDialog`]）で共用する。
/// PDF（印刷）は、印刷する中身が呼び出し側で違うので、ここでは持たない（[`ExportPrint`]）。
///
/// 出典に社内文書を持つ内容は、閲覧権限のある人だけが読める文書の抜粋を含みうる。ドライブに置くと
/// 共有で出典を読めない人にも見えるので、保存ダイアログで注意を出す（human 決定: UI の注意に留める）。

import Link from "next/link";
import { useRouter } from "next/navigation";
import * as React from "react";

import { toast } from "@/components/ui/use-toast";
import {
  SaveDraftDialog,
  SOURCES_NOTICE,
  saveFailedMessage,
  type SaveTarget,
} from "@/components/notes/save-draft-dialog";
import { docxMarkdown, noteMarkdown } from "@/lib/answer-export";
import { markdownToDocx } from "@/lib/documents-api";
import { downloadBlob } from "@/lib/download";
import { useDefaultSaveFolder } from "@/lib/drafts/save-folder";
import { draftHref } from "@/lib/notes/draft-nav";
import { getDraft, upsertDraft } from "@/lib/notes/draft-store";
import { uploadNewFile } from "@/lib/storage";

const DOCX_TYPE = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";

export type ExportActions = {
  /// Word をダウンロードするための変換中（その項目だけ押せなくする）。
  converting: boolean;
  /// Word をドライブに保存している最中（その項目だけ押せなくする）。
  saving: boolean;
  openAsNote: () => Promise<void>;
  downloadMd: () => Promise<void>;
  downloadDocx: () => Promise<void>;
  /// 名前と保存先を選ぶダイアログを開く（保存は確定したとき）。
  openSaveDocx: () => Promise<void>;
  /// 保存ダイアログ（呼び出し側が描く）。
  dialog: React.ReactNode;
};

export function useExportActions({
  threadId,
  what,
  fileName,
  markdown,
  hasDocSources,
}: {
  threadId: string;
  /// ダイアログの説明に出す呼び名（「回答」「会話」）。
  what: string;
  /// 書き出すファイルの名前（拡張子なし）。押したときに求める。
  fileName: () => Promise<string> | string;
  /// 書き出す Markdown（出典の一覧を含む）。押したときに組み立てる。
  markdown: () => Promise<string>;
  hasDocSources: boolean;
}): ExportActions {
  const router = useRouter();
  const [converting, setConverting] = React.useState(false);
  const [saveOpen, setSaveOpen] = React.useState(false);
  const [saving, setSaving] = React.useState(false);
  const [saveName, setSaveName] = React.useState("");
  const defaultFolder = useDefaultSaveFolder(threadId, undefined, saveOpen);

  const toDocx = async (name: string) =>
    markdownToDocx({ name, markdown: await docxMarkdown(await markdown()) });

  const openAsNote = async () => {
    // 同じ会話で同じ名前の下書きがあれば、上書きせず連番の名前で開く。
    const name = await fileName();
    let draftName = name;
    for (let i = 2; getDraft(threadId, draftName); i++) draftName = `${name} (${i})`;
    upsertDraft(threadId, draftName, await noteMarkdown(await markdown()), "user", undefined, {
      sourcesNotice: hasDocSources,
    });
    router.push(draftHref(threadId, draftName));
  };

  const downloadMd = async () => {
    const [md, name] = await Promise.all([markdown(), fileName()]);
    downloadBlob(new Blob([md], { type: "text/markdown;charset=utf-8" }), `${name}.md`);
  };

  const downloadDocx = async () => {
    if (converting) return;
    setConverting(true);
    try {
      const name = await fileName();
      downloadBlob(await toDocx(name), `${name}.docx`);
    } catch (e) {
      toast({ variant: "destructive", description: errorText(e, "Word への変換に失敗しました。") });
    } finally {
      setConverting(false);
    }
  };

  const openSaveDocx = async () => {
    setSaveName(await fileName());
    setSaveOpen(true);
  };

  const saveDocx = async (target: SaveTarget) => {
    if (saving) return;
    setSaving(true);
    const docxName = /\.docx$/i.test(target.name) ? target.name : `${target.name}.docx`;
    let blob: Blob;
    try {
      blob = await toDocx(docxName.replace(/\.docx$/i, ""));
    } catch (e) {
      toast({ variant: "destructive", description: errorText(e, "Word への変換に失敗しました。") });
      setSaving(false);
      return;
    }
    try {
      // 同じ名前があれば連番の名前で置く（既存のファイルを上書きしない）。
      const node = await uploadNewFile({
        file: new File([blob], docxName, { type: DOCX_TYPE }),
        parentId: target.parentId ?? undefined,
      });
      setSaveOpen(false);
      toast({
        description: (
          <span>
            「{node.name}」をドライブに保存しました。
            <Link href={`/office/${node.id}`} className="ml-1 font-medium underline underline-offset-2">
              開く
            </Link>
          </span>
        ),
      });
    } catch (e) {
      toast({ variant: "destructive", description: saveFailedMessage(e, target.parentId) });
    } finally {
      setSaving(false);
    }
  };

  const dialog = (
    <SaveDraftDialog
      open={saveOpen}
      // 保存中でも閉じられる（通信が止まっても会話を続けられる）。保存はそのまま続き、結果はトーストで
      // 知らせる。終わるまでその項目は押せない（同じ内容を二重に保存しない）。
      onOpenChange={setSaveOpen}
      defaultName={saveName}
      saving={saving}
      onConfirm={(t) => void saveDocx(t)}
      entityLabel="ファイル"
      description={`${what}を Word 文書（.docx）にしてドライブに保存します。`}
      defaultFolder={defaultFolder}
      notice={hasDocSources ? SOURCES_NOTICE : undefined}
    />
  );

  return { converting, saving, openAsNote, downloadMd, downloadDocx, openSaveDocx, dialog };
}

function errorText(e: unknown, fallback: string): string {
  return e instanceof Error && e.message ? e.message : fallback;
}
