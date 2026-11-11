"use client";

/// PDF の抜粋画像を、遷移先の入力欄へ一度だけ引き渡す（永続化しない）。
import * as React from "react";
import type { Attachment } from "@/lib/chat-api";
import { storageScopeGeneration } from "@/lib/user-storage";

let pending: { files: Attachment[]; generation: number } | null = null;
const listeners = new Set<() => void>();
export function queuePdfAttachments(files: Attachment[]) {
  pending = { files, generation: storageScopeGeneration() };
  listeners.forEach((l) => l());
}
export function usePdfAttachments() {
  return React.useSyncExternalStore(
    (listener) => {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    () => pending,
    () => null,
  );
}
export function takePdfAttachments() {
  const out = pending;
  pending = null;
  listeners.forEach((l) => l());
  return out?.generation === storageScopeGeneration() ? out.files : [];
}
