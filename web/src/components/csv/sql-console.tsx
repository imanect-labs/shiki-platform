"use client";

/// CSV の読み取り専用 SQL コンソール（Task 11P.8）。
///
/// テーブル名は `data`。結果は簡易テーブルで表示し、「新規 CSV として保存」を明示操作で提供。
/// 実行は tabular サービス（11P.7・隔離 DuckDB）経由の RO SQL。
/// 新規 CSV は元の CSV と同じフォルダーに保存する。そのフォルダーを見られない・書き込めないときはドライブのトップに
/// 保存する（GAP-66）。

import { Play, Save } from "lucide-react";
import * as React from "react";

import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { toast } from "@/components/ui/use-toast";
import { breadcrumb, getNode } from "@/lib/storage";
import {
  CsvFolderUnavailable,
  CsvNameTaken,
  csvFileName,
  runQuery,
  saveNewCsv,
  tableToCsv,
  type TableResponse,
} from "@/lib/tabular-api";

/// 新規 CSV の保存先（GAP-66）。
///
/// - `folder`: 元の CSV と同じフォルダー。
/// - `top`: ドライブのトップ。`fallback` は、元の CSV がフォルダーにあるのに、そこへ保存できないとき（フォルダーを
///   見られない・書き込めない）。
type SaveDestination =
  | { kind: "loading" }
  | { kind: "folder"; id: string; name: string }
  | { kind: "top"; fallback: boolean };

/// 元の CSV のある場所から保存先を決める。
///
/// - フォルダーを見られない人（CSV だけを共有された人）はトップにする（パンくずに親が入らない）。
/// - フォルダーの編集者は中のファイルの編集者でもあるので、CSV を閲覧だけで開いている人もトップにする。
/// - CSV を編集できても、フォルダーに書き込めないことはある（フォルダーは閲覧だけで共有された人）。そのときは保存の
///   応答で分かるので、トップに保存し直す（[`CsvFolderUnavailable`]）。
/// - 場所を読めなければトップにする。
function useSaveDestination(nodeId: string, canEditFile: boolean): [SaveDestination, (d: SaveDestination) => void] {
  const [dest, setDest] = React.useState<SaveDestination>({ kind: "loading" });
  React.useEffect(() => {
    let active = true;
    setDest({ kind: "loading" });
    Promise.all([getNode(nodeId), breadcrumb(nodeId).catch(() => [])])
      .then(([node, crumbs]) => {
        if (!active) return;
        if (!node.parent_id) {
          setDest({ kind: "top", fallback: false });
          return;
        }
        // パンくずは読めるところまで（読めない親は入らない）。最後が CSV 自身、その前が親のフォルダー。
        const parent = crumbs.length >= 2 ? crumbs[crumbs.length - 2] : null;
        setDest(
          canEditFile && parent && parent.id === node.parent_id
            ? { kind: "folder", id: parent.id, name: parent.name }
            : { kind: "top", fallback: true },
        );
      })
      .catch(() => {
        if (active) setDest({ kind: "top", fallback: false });
      });
    return () => {
      active = false;
    };
  }, [nodeId, canEditFile]);
  return [dest, setDest];
}

/// 保存のダイアログの説明（保存先を言う）。
function destinationText(dest: SaveDestination): string {
  switch (dest.kind) {
    case "loading":
      return "保存先を確かめています…";
    case "folder":
      return `結果を新しい CSV として、この CSV と同じ「${dest.name}」フォルダーに保存します。`;
    case "top":
      return dest.fallback
        ? "この CSV のフォルダーには保存できないため、結果を新しい CSV として、ドライブのトップに保存します。"
        : "結果を新しい CSV として、ドライブのトップに保存します。";
  }
}

export function SqlConsole({
  nodeId,
  canEditFile,
  onSaved,
}: {
  nodeId: string;
  /// 開いている CSV を編集できるか（閲覧だけなら、そのフォルダーにも書き込めない）。
  canEditFile: boolean;
  onSaved?: (savedNodeId: string) => void;
}) {
  const [sql, setSql] = React.useState("SELECT * FROM data LIMIT 100");
  const [result, setResult] = React.useState<TableResponse | null>(null);
  const [running, setRunning] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);
  const [elapsedMs, setElapsedMs] = React.useState<number | null>(null);
  const [saveOpen, setSaveOpen] = React.useState(false);
  const [saveName, setSaveName] = React.useState("query-result");
  const [savingCsv, setSavingCsv] = React.useState(false);
  // 保存先に同じ名前のファイルがあった（名前を変えるか、番号を付けて保存する・BUG-150）。知らせはその名前のときだけ出す
  // （名前を変えれば消え、戻せばまた出る）。
  const [takenError, setTakenError] = React.useState<CsvNameTaken | null>(null);
  const nameTaken = takenError && takenError.fileName === csvFileName(saveName.trim()) ? takenError : null;
  const [dest, setDest] = useSaveDestination(nodeId, canEditFile);

  // 数値列（右寄せ表示・表計算ソフトの体裁）。結果のサンプルから判定する。
  const numericCols = React.useMemo(() => {
    const nums = new Set<number>();
    if (!result) return nums;
    for (let c = 0; c < result.columns.length; c++) {
      let sawValue = false;
      let allNumeric = true;
      for (const row of result.rows) {
        const v = row[c];
        if (v == null || v === "") continue;
        sawValue = true;
        if (!Number.isFinite(Number(v.replace(/,/g, "")))) {
          allNumeric = false;
          break;
        }
      }
      if (sawValue && allNumeric) nums.add(c);
    }
    return nums;
  }, [result]);

  const run = async () => {
    setRunning(true);
    setError(null);
    const started = performance.now();
    try {
      const res = await runQuery(nodeId, sql);
      setResult(res);
      setElapsedMs(Math.round(performance.now() - started));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setResult(null);
      setElapsedMs(null);
    } finally {
      setRunning(false);
    }
  };

  /// `addNumber` は同じ名前があったときに「名前 (2).csv」のように番号を付けて保存する（サーバーが空いている名前を探す）。
  const saveResult = async (addNumber = false) => {
    // savingCsv 中は二重発火を防ぐ（Enter と 保存ボタンの多重呼び出し対策）。保存先が決まるまでは保存しない。
    if (!result || !saveName.trim() || savingCsv || dest.kind === "loading") return;
    setSavingCsv(true);
    const save = (parentId: string | null) =>
      saveNewCsv({
        parentId,
        name: saveName.trim(),
        csv: tableToCsv(result),
        onNameTaken: addNumber ? "add_number" : undefined,
      });
    const folder = dest.kind === "folder" ? dest : null;
    try {
      let saved: Awaited<ReturnType<typeof saveNewCsv>>;
      let note: string;
      try {
        saved = await save(folder?.id ?? null);
        note = folder
          ? `「${saved.name}」を「${folder.name}」フォルダーに作成しました。`
          : `「${saved.name}」を作成しました。`;
      } catch (e) {
        // 元の CSV のフォルダーに書き込めない（フォルダーは閲覧だけの人）・その間に消された: トップに保存し直す。
        // トップに同じ名前があれば、下の catch で名前の欄の下に言う（説明もトップに変わる）。
        if (!folder || !(e instanceof CsvFolderUnavailable)) throw e;
        setDest({ kind: "top", fallback: true });
        saved = await save(null);
        note = `「${folder.name}」フォルダーには保存できないため、ドライブのトップに「${saved.name}」を作成しました。`;
      }
      toast({ title: "保存しました", description: note });
      setSaveOpen(false);
      setTakenError(null);
      onSaved?.(saved.node_id);
    } catch (e) {
      // 同じ名前は欄の下で言い、番号を付けて保存するボタンを出す（ほかの人の編集と取り違えない）。
      if (e instanceof CsvNameTaken) {
        setTakenError(e);
        return;
      }
      toast({
        variant: "destructive",
        title: "保存できませんでした",
        description: e instanceof Error ? e.message : String(e),
      });
    } finally {
      setSavingCsv(false);
    }
  };

  return (
    <div className="flex h-full min-h-0 flex-col gap-2 p-3 max-md:px-1" data-testid="sql-console">
      <div className="flex items-start gap-2">
        <textarea
          value={sql}
          onChange={(e) => setSql(e.target.value)}
          onKeyDown={(e) => {
            if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
              e.preventDefault();
              void run();
            }
          }}
          rows={3}
          spellCheck={false}
          autoCapitalize="none"
          autoCorrect="off"
          aria-label="SQL"
          data-testid="sql-input"
          className="scrollbar-subtle min-h-16 min-w-0 flex-1 resize-y rounded-md border bg-background p-2 font-mono text-base focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring md:text-sm"
        />
        <div className="flex flex-col gap-2">
          <Button
            type="button"
            size="sm"
            loading={running}
            onClick={() => void run()}
            disabled={running}
            data-testid="sql-run"
          >
            {!running ? <Play className="size-4" aria-hidden /> : null}
            実行
          </Button>
          <Button
            type="button"
            size="sm"
            variant="outline"
            onClick={() => setSaveOpen(true)}
            disabled={!result}
            data-testid="sql-save"
          >
            <Save className="size-4" aria-hidden />
            新規 CSV
          </Button>
          {/* キーボードショートカットの案内はマウスの端末だけ（タッチでは押せない）。 */}
          <kbd className="rounded border bg-muted/50 px-1.5 py-0.5 text-center text-[10px] font-medium text-muted-foreground pointer-coarse:hidden">
            ⌘↵ で実行
          </kbd>
        </div>
      </div>
      {error && (
        <p
          className="rounded-md border border-destructive/40 bg-destructive/5 px-3 py-2 text-sm text-destructive"
          data-testid="sql-error"
        >
          {error}
        </p>
      )}
      {result && (
        <div className="flex items-center gap-2 px-0.5 text-xs text-muted-foreground">
          <span className="font-medium text-foreground/70">{result.rows.length} 行</span>
          {elapsedMs != null ? <span>· {elapsedMs} ms</span> : null}
          {result.truncated ? (
            <span className="text-[color:var(--season-autumn)]">· 上限で打ち切り</span>
          ) : null}
        </div>
      )}
      {result && (
        <div className="scrollbar-subtle min-h-0 flex-1 overflow-auto overscroll-contain rounded-lg border shadow-xs">
          <table className="w-full border-collapse text-sm">
            <thead className="sticky top-0 bg-muted/60">
              <tr>
                {result.columns.map((c, j) => (
                  <th
                    key={c}
                    className={`border-b px-3 py-1.5 font-semibold ${numericCols.has(j) ? "text-right" : "text-left"}`}
                  >
                    {c}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {result.rows.map((row, i) => (
                <tr key={i} className="odd:bg-muted/20">
                  {row.map((cell, j) => (
                    <td
                      key={j}
                      className={`border-b px-3 py-1 tabular-nums ${numericCols.has(j) ? "text-right" : "text-left"}`}
                    >
                      {cell ?? ""}
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
          {result.truncated && (
            <p className="px-3 py-2 text-xs text-muted-foreground">
              結果が上限で打ち切られました（全件は絞り込んでください）。
            </p>
          )}
        </div>
      )}

      {/* 新規 CSV として保存（window.prompt の代わりにアプリの Dialog を使う） */}
      <Dialog
        open={saveOpen}
        onOpenChange={(o) => {
          setSaveOpen(o);
          setTakenError(null);
        }}
      >
        <DialogContent data-testid="sql-save-dialog">
          <DialogHeader>
            <DialogTitle>新規 CSV として保存</DialogTitle>
            <DialogDescription className="[overflow-wrap:anywhere]" data-testid="sql-save-destination">
              {destinationText(dest)}
            </DialogDescription>
          </DialogHeader>
          <div className="flex flex-col gap-1.5">
            <label htmlFor="sql-save-name" className="text-sm font-medium">
              ファイル名
            </label>
            <Input
              id="sql-save-name"
              value={saveName}
              onChange={(e) => setSaveName(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault();
                  // 同じ名前の知らせが出ている間は、Enter で番号を付けた保存をしない（ボタンで選ぶ）。
                  if (!nameTaken) void saveResult();
                }
              }}
              aria-invalid={nameTaken ? true : undefined}
              aria-describedby={nameTaken ? "sql-save-name-taken" : undefined}
              autoFocus
              autoCapitalize="none"
              autoCorrect="off"
              spellCheck={false}
              enterKeyHint="done"
              placeholder="query-result"
            />
            {nameTaken ? (
              <p
                id="sql-save-name-taken"
                role="alert"
                className="text-sm text-destructive"
                data-testid="sql-save-name-taken"
              >
                {nameTaken.message}
              </p>
            ) : null}
          </div>
          <DialogFooter>
            <Button
              variant="ghost"
              onClick={() => {
                setSaveOpen(false);
                setTakenError(null);
              }}
            >
              キャンセル
            </Button>
            {nameTaken && !nameTaken.numbered ? (
              <Button loading={savingCsv} onClick={() => void saveResult(true)} data-testid="sql-save-numbered">
                番号を付けて保存
              </Button>
            ) : (
              <Button
                loading={savingCsv}
                disabled={!saveName.trim() || nameTaken !== null || dest.kind === "loading"}
                onClick={() => void saveResult()}
              >
                保存
              </Button>
            )}
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
