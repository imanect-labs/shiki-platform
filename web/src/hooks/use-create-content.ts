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
/// - Excel(.xlsx): POST /sheets（blank.xlsx テンプレはサーバ正本・#381。変換なし）
/// - スライド: POST /slides（同名はサーバ側連番リネーム）
/// - CSV: POST /tabular/save（同名はサーバ側でノートと同じ規則の連番リネーム・`on_name_taken: add_number`）

import * as React from "react";
import { useRouter } from "next/navigation";

import { toast } from "@/components/ui/use-toast";
import { createDocument, createSheet } from "@/lib/documents-api";
import { createNote } from "@/lib/notes-api";
import { createSlide } from "@/lib/slides-api";
import { CsvNameTaken, saveNewCsv } from "@/lib/tabular-api";

export type CreateContentKind = "note" | "document" | "sheet" | "slide" | "csv";

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

  // Excel ブック（xlsx・#381）: サーバ側テンプレから作成して Collabora Calc へ遷移する。
  // CSV（グリッド＋SQL 分析）とは別物として並べる — 「Excel が欲しい」に CSV を出さない。
  const createSheetAndOpen = React.useCallback(
    () =>
      run("sheet", "Excel ブックの作成に失敗しました", async () => {
        const node = await createSheet({
          parentId: parentId ?? undefined,
          name: "無題のブック",
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

  // CSV（グリッドエディタ・Task 11P.8）: ヘッダのみの空 CSV。同名はノート・スライドと同じく
  // サーバが「無題のスプレッドシート (2).csv」のように番号を付けて保存する（D-112）。
  const createCsvAndOpen = React.useCallback(
    () =>
      run("csv", "CSV の作成に失敗しました", async () => {
        try {
          const saved = await saveNewCsv({
            parentId: parentId ?? undefined,
            name: "無題のスプレッドシート",
            csv: "列1,列2,列3\n,,\n",
            onNameTaken: "add_number",
          });
          return `/csv/${saved.node_id}`;
        } catch (e) {
          if (!(e instanceof CsvNameTaken)) throw e;
          throw new Error(
            "このフォルダーに「無題のスプレッドシート」の CSV が多すぎるため、作成できませんでした。いくつか名前を変えるか削除してから、もう一度作成してください。",
          );
        }
      }),
    [run, parentId],
  );

  return {
    createNoteAndOpen,
    createDocumentAndOpen,
    createSheetAndOpen,
    createSlideAndOpen,
    createCsvAndOpen,
    /// 実行中の種別（ボタンの二重発火防止・スピナー表示用）。
    creating,
  };
}
