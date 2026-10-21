"use client";

/// ドライブのアップロード（D&D / ボタン / 新しいバージョン）の進行と同名衝突の扱い。
///
/// - 進捗は 1 件ごとの一意 ID で持つ（iOS は写真を選ぶと全部 `image.jpg` になり、名前で
///   引くと進捗と完了が別のファイルに混線するため）。
/// - 送る前に同じフォルダの同名を確かめ（`findSiblingByName`）、衝突したものは送らずに
///   まとめて利用者に聞く（新しいバージョン／両方残す／スキップ）。送ってから 409 で
///   捨てると大きなファイルほど無駄になる。見えない同名（読めない・直前に作られた）は
///   finalize の 409 で気付き、同じ問い合わせに合流させる。
/// - 1 回の選択の中で同じ名前が重なるもの（iOS の `image.jpg` 等）は、別物として選ばれて
///   いるのが明らかなので聞かずに連番名で両方残す。
/// - 失敗は 1 件ずつトーストを出さず、1 件にまとめる（「3 件をアップロードできませんでした」）。

import * as React from "react";

import { toast } from "@/components/ui/use-toast";
import {
  findSiblingByName,
  isConflict,
  uploadFile,
  type NodeResponse,
} from "@/lib/storage";

export type UploadProgress = { id: string; name: string; fraction: number };

/// 同名衝突 1 件。`existing` は同じフォルダにある同名ノード（読めない等で特定できなければ null）。
export type UploadConflict = { id: string; file: File; existing: NodeResponse | null };

export type ConflictResolution = "version" | "both" | "skip";

type Failure = { name: string; message: string };

/// 「議事録.md」→「議事録 (1).md」。拡張子の前に連番を入れる（先頭ドットだけの名前は拡張子扱いしない）。
export function numberedName(name: string, n: number): string {
  const dot = name.lastIndexOf(".");
  if (dot <= 0) return `${name} (${n})`;
  return `${name.slice(0, dot)} (${n})${name.slice(dot)}`;
}

const MAX_RENAME_TRIES = 50;

export function useDriveUploads({
  folderId,
  onChanged,
}: {
  folderId: string | null;
  /// 1 件以上反映されたら呼ぶ（一覧の再読込）。
  onChanged: () => void;
}) {
  const [uploads, setUploads] = React.useState<UploadProgress[]>([]);
  const [conflicts, setConflicts] = React.useState<UploadConflict[]>([]);
  const seq = React.useRef(0);
  const nextId = () => `up-${++seq.current}`;
  const onChangedRef = React.useRef(onChanged);
  onChangedRef.current = onChanged;
  // 衝突の問い合わせ中に別のフォルダへ移っても、選んだ時点のフォルダへ保存する。
  const conflictFolderRef = React.useRef<string | null>(folderId);

  /// 進捗行を出して 1 件送る（成功・失敗にかかわらず行は消す）。
  const send = React.useCallback(
    async (
      file: File,
      opts: { parentId: string | null; name?: string; targetNodeId?: string },
    ): Promise<NodeResponse> => {
      const id = nextId();
      setUploads((prev) => [...prev, { id, name: opts.name ?? file.name, fraction: 0 }]);
      try {
        return await uploadFile({
          file,
          parentId: opts.parentId ?? undefined,
          name: opts.name,
          targetNodeId: opts.targetNodeId,
          onProgress: (fraction) =>
            setUploads((prev) => prev.map((u) => (u.id === id ? { ...u, fraction } : u))),
        });
      } finally {
        setUploads((prev) => prev.filter((u) => u.id !== id));
      }
    },
    [],
  );

  /// 空いている連番名を探して送る（送る前に確かめ、それでも 409 なら次の番号へ）。
  const sendWithFreeName = React.useCallback(
    async (file: File, parentId: string | null, taken: Set<string>) => {
      for (let n = 1; n <= MAX_RENAME_TRIES; n += 1) {
        const candidate = numberedName(file.name, n);
        if (taken.has(candidate)) continue;
        if (await findSiblingByName(parentId, candidate).catch(() => null)) continue;
        try {
          taken.add(candidate);
          return await send(file, { parentId, name: candidate });
        } catch (e) {
          if (!isConflict(e)) throw e;
        }
      }
      throw new Error("空いている名前が見つかりませんでした");
    },
    [send],
  );

  const report = (succeeded: number, failures: Failure[], what = "アップロード") => {
    if (failures.length > 0) {
      const names = failures.slice(0, 3).map((f) => `「${f.name}」`).join("");
      const more = failures.length > 3 ? ` ほか ${failures.length - 3} 件` : "";
      // 件数が多くても 1 件のトーストにまとめ、名前は先頭 3 件と先頭の理由だけ添える。
      const reason = failures[0].message;
      toast({
        variant: "destructive",
        title:
          failures.length === 1
            ? `「${failures[0].name}」を${what}できませんでした`
            : `${failures.length} 件を${what}できませんでした`,
        description: failures.length === 1 ? reason : `${names}${more}：${reason}`,
      });
    }
    if (succeeded > 0) {
      toast({
        title:
          failures.length === 0
            ? succeeded === 1
              ? `${what}が完了しました`
              : `${succeeded} 件を${what}しました`
            : `${succeeded} 件を${what}しました（${failures.length} 件は失敗）`,
      });
      onChangedRef.current();
    }
  };

  /// 選ばれたファイル群を送る。衝突したものは `conflicts` に積み、問い合わせを待つ。
  const uploadFiles = React.useCallback(
    async (files: File[]) => {
      if (files.length === 0) return;
      const parentId = folderId;
      // 同じ選択の中で名前が重なるものは 2 件目以降を連番名にする（聞かない）。
      const seen = new Map<string, number>();
      const jobs = files.map((file) => {
        const count = seen.get(file.name) ?? 0;
        seen.set(file.name, count + 1);
        return { file, renameInBatch: count > 0 };
      });
      // 先に同名の有無を確かめる（並列・各 1 リクエスト）。確認自体の失敗は「衝突なし」として
      // 送り、本当に衝突していれば finalize の 409 で拾う。
      const existing = await Promise.all(
        jobs.map((j) =>
          j.renameInBatch ? Promise.resolve(null) : findSiblingByName(parentId, j.file.name).catch(() => null),
        ),
      );

      let succeeded = 0;
      const failures: Failure[] = [];
      const pending: UploadConflict[] = [];
      const taken = new Set(files.map((f) => f.name));
      for (const [i, job] of jobs.entries()) {
        const hit = existing[i];
        if (hit) {
          pending.push({ id: nextId(), file: job.file, existing: hit });
          continue;
        }
        try {
          if (job.renameInBatch) await sendWithFreeName(job.file, parentId, taken);
          else await send(job.file, { parentId });
          succeeded += 1;
        } catch (e) {
          if (isConflict(e)) {
            const found = await findSiblingByName(parentId, job.file.name).catch(() => null);
            pending.push({ id: nextId(), file: job.file, existing: found });
          } else {
            failures.push({ name: job.file.name, message: e instanceof Error ? e.message : String(e) });
          }
        }
      }
      report(succeeded, failures);
      if (pending.length > 0) {
        conflictFolderRef.current = parentId;
        setConflicts((prev) => [...prev, ...pending]);
      }
    },
    [folderId, send, sendWithFreeName],
  );

  /// 衝突の問い合わせへの回答を全件に当てる。
  const resolveConflicts = React.useCallback(
    async (resolution: ConflictResolution) => {
      const items = conflicts;
      setConflicts([]);
      if (resolution === "skip" || items.length === 0) return;
      const parentId = conflictFolderRef.current;
      let succeeded = 0;
      const failures: Failure[] = [];
      const taken = new Set<string>();
      for (const c of items) {
        try {
          // 新しいバージョンは既存の「ファイル」にだけ積める。フォルダと同名・特定できない
          // 同名は版にできないので、名前を変えて両方残す。
          if (resolution === "version" && c.existing && c.existing.kind !== "folder") {
            await send(c.file, { parentId, targetNodeId: c.existing.id });
          } else {
            await sendWithFreeName(c.file, parentId, taken);
          }
          succeeded += 1;
        } catch (e) {
          failures.push({ name: c.file.name, message: e instanceof Error ? e.message : String(e) });
        }
      }
      report(succeeded, failures, resolution === "version" ? "保存" : "アップロード");
    },
    [conflicts, send, sendWithFreeName],
  );

  /// 既存ファイルへ新しいバージョンを送る（行メニューの「新しいバージョン」）。
  const uploadNewVersion = React.useCallback(
    async (file: File, node: NodeResponse) => {
      try {
        await send(file, { parentId: node.parent_id ?? null, targetNodeId: node.id });
        toast({ title: "新しいバージョンをアップロードしました", description: node.name });
        onChangedRef.current();
      } catch (e) {
        toast({
          variant: "destructive",
          title: "バージョンの追加に失敗しました",
          description: e instanceof Error ? e.message : String(e),
        });
      }
    },
    [send],
  );

  return { uploads, conflicts, uploadFiles, resolveConflicts, uploadNewVersion };
}
