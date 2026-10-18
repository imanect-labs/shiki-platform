"use client";

/// CSV 詳細のクローム（シェルの `usePageChrome` へ登録する）。
///
/// - 左: 戻る。右: 保存（主操作・編集者のみ）→ 表示切替（スマホはアイコン 1 つ）→ ⋯（ダウンロード）。
/// - PC のバー: 名前＋「N 行 × M 列」＋閲覧のみ、補助にグリッド/SQL の切替。
/// - スマホ: 名前と状態は本文先頭（ページ側の EditorMobileHeading）に出す。
///
/// 読み込み中・notfound でも戻るを出すため、ページ最上位（早期 return より前）で呼ぶ。

import { Download, Eye, Save, Table2, Terminal } from "lucide-react";

import { EditorStatusBadge, useEditorBack } from "@/components/shell/editor-loading";
import {
  ChromeIconButton,
  ChromePrimaryButton,
  usePageChrome,
} from "@/components/shell/page-header-context";
import { SegmentedControl } from "@/components/ui/segmented-control";

export type CsvTab = "grid" | "sql";

export type CsvChromeState = {
  name: string;
  totalRows: number;
  cols: number;
  editable: boolean;
  tab: CsvTab;
  onTabChange: (t: CsvTab) => void;
  dirty: boolean;
  saving: boolean;
  onSave: () => void;
  onDownload: () => void;
};

/// クロームを登録する。`state` が null の間（読み込み中・開けない）は戻るだけを出す。
export function useCsvChrome(state: CsvChromeState | null): void {
  const back = useEditorBack();
  const name = state?.name;
  const totalRows = state?.totalRows ?? 0;
  const cols = state?.cols ?? 0;
  const editable = state?.editable ?? false;
  const tab = state?.tab ?? "grid";
  const onTabChange = state?.onTabChange;
  const dirty = state?.dirty ?? false;
  const saving = state?.saving ?? false;
  const onSave = state?.onSave;
  const onDownload = state?.onDownload;

  usePageChrome(() => {
    if (!onTabChange || !onSave || !onDownload) return { back };
    const nextTab: CsvTab = tab === "grid" ? "sql" : "grid";
    return {
      title: name,
      menuTitle: name,
      back,
      subtitle: (
        <span className="inline-flex items-center gap-2">
          <span className="tabular-nums">
            {totalRows} 行 × {cols} 列
          </span>
          {!editable ? (
            <EditorStatusBadge icon={Eye} data-testid="csv-readonly-badge">
              閲覧のみ
            </EditorStatusBadge>
          ) : null}
        </span>
      ),
      desktopExtra: (
        <SegmentedControl
          aria-label="表示切替"
          value={tab}
          onValueChange={(v) => onTabChange(v as CsvTab)}
          className="mr-1"
          options={[
            { value: "grid", label: "グリッド", icon: Table2, testId: "csv-tab-grid" },
            { value: "sql", label: "SQL", icon: Terminal, testId: "csv-tab-sql" },
          ]}
        />
      ),
      primary: editable ? (
        <ChromePrimaryButton
          onClick={onSave}
          disabled={!dirty || saving}
          aria-busy={saving || undefined}
          data-testid="csv-save"
        >
          <Save aria-hidden />
          {saving ? "保存中…" : `保存${dirty ? "*" : ""}`}
        </ChromePrimaryButton>
      ) : undefined,
      // スマホ: グリッド ⇄ SQL を 1 つのアイコンで切り替える（PC はバーの切替を使う）。
      actions: (
        <ChromeIconButton
          label={nextTab === "sql" ? "SQL コンソールを開く" : "グリッドに戻る"}
          onClick={() => onTabChange(nextTab)}
          className="md:hidden"
          data-testid="csv-tab-toggle"
        >
          {nextTab === "sql" ? <Terminal aria-hidden /> : <Table2 aria-hidden />}
        </ChromeIconButton>
      ),
      menu: [
        {
          key: "download",
          label: "CSV をダウンロード",
          icon: Download,
          onSelect: onDownload,
          testId: "csv-menu-download",
        },
      ],
    };
  }, [back, name, totalRows, cols, editable, tab, onTabChange, dirty, saving, onSave, onDownload]);
}
