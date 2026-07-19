"use client";

/// 新規コンテンツ（ノート/ドキュメント/スライド/CSV）の「作成→エディタへ遷移」フック（#333）。
///
/// ドライブの「新規作成」メニューとチャット「+」の「作成」サブメニューで共用する
/// （作成ロジックを 1 箇所に集約・重複実装しない）。保存先は `parentId`（未指定は
/// マイドライブ直下）。失敗は toast で通知し、成功時は対応エディタへ遷移する。
///
/// - ノート: POST /notes（同名はサーバ側 create_file_unique が連番リネーム）
/// - ドキュメント(.docx): POST /documents（blank.docx テンプレはサーバ正本・#332。
///   markdown 省略なので ingestion-worker 非稼働でも作成できる）
/// - スライド: POST /slides（同名はサーバ側連番リネーム）
/// - CSV: POST /tabular/save（同名 409 はクライアント側で連番リトライ）

import * as React from "react";
import { useRouter } from "next/navigation";

import { toast } from "@/components/ui/use-toast";
import { createDocument } from "@/lib/documents-api";
import { createNote } from "@/lib/notes-api";
import { createSlide } from "@/lib/slides-api";
import { saveNewCsv, TabularConflict } from "@/lib/tabular-api";

export type CreateContentKind = "note" | "document" | "slide" | "csv";

export function useCreateContent({ parentId }: { parentId?: string | null }) {
  const router = useRouter();
  const [creating, setCreating] = React.useState<CreateContentKind | null>(null);
  // 遷移後のアンマウント越しに setState しない（creating は遷移で画面ごと消えるため
  // 成功パスでは解除しない選択もあるが、panel 内共用を考え finally で常に解除する）。
  const creatingRef = React.useRef<CreateContentKind | null>(null);

  const run = React.useCallback(
    async (kind: CreateContentKind, title: string, fn: () => Promise<string>) => {
      if (creatingRef.current) return;
      creatingRef.current = kind;
      setCreating(kind);
      try {
        const href = await fn();
        router.push(href);
      } catch (e) {
        toast({
          variant: "destructive",
          title,
          description: e instanceof Error ? e.message : String(e),
        });
      } finally {
        creatingRef.current = null;
        setCreating(null);
      }
    },
    [router],
  );

  const createNoteAndOpen = React.useCallback(
    () =>
      run("note", "ノートの作成に失敗しました", async () => {
        const note = await createNote({ parentId: parentId ?? undefined, name: "無題のノート" });
        return `/notes/${note.id}`;
      }),
    [run, parentId],
  );

  // ドキュメント（docx・Office 統合 Task 11.7/#332）: サーバ側テンプレから作成して
  // Collabora エディタへ遷移する（Office 未配備なら /office 側が案内表示にフォールバック）。
  const createDocumentAndOpen = React.useCallback(
    () =>
      run("document", "ドキュメントの作成に失敗しました", async () => {
        const node = await createDocument({
          parentId: parentId ?? undefined,
          name: "無題のドキュメント",
        });
        return `/office/${node.id}`;
      }),
    [run, parentId],
  );

  // スライド（自前実装・Task 11.1）を作成してビューアへ遷移する。
  const createSlideAndOpen = React.useCallback(
    () =>
      run("slide", "スライドの作成に失敗しました", async () => {
        const slide = await createSlide({ parentId: parentId ?? undefined, name: "無題のスライド" });
        return `/slides/${slide.id}`;
      }),
    [run, parentId],
  );

  // CSV（グリッドエディタ・Task 11P.8）: ヘッダのみの空 CSV。同名は 409 のため連番で
  // 空きを探す（2 回目以降の作成が黙って失敗しない）。
  const createCsvAndOpen = React.useCallback(
    () =>
      run("csv", "CSV の作成に失敗しました", async () => {
        let saved: Awaited<ReturnType<typeof saveNewCsv>> | null = null;
        for (let n = 1; n <= 20 && !saved; n++) {
          const name = n === 1 ? "無題のスプレッドシート" : `無題のスプレッドシート ${n}`;
          try {
            saved = await saveNewCsv({
              parentId: parentId ?? undefined,
              name,
              csv: "列1,列2,列3\n,,\n",
            });
          } catch (e) {
            if (!(e instanceof TabularConflict)) throw e;
          }
        }
        if (!saved) {
          throw new Error("同名のスプレッドシートが多すぎます。名前を変えて作成してください。");
        }
        return `/csv/${saved.node_id}`;
      }),
    [run, parentId],
  );

  return {
    createNoteAndOpen,
    createDocumentAndOpen,
    createSlideAndOpen,
    createCsvAndOpen,
    /// 実行中の種別（ボタンの二重発火防止・スピナー表示用）。
    creating,
  };
}
