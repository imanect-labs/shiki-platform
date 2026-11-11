"use client";

/// PDF の抜粋画像を、遷移先の入力欄へ一度だけ引き渡す（永続化しない）。
import * as React from "react";
import type { Attachment } from "@/lib/chat-api";
import { storageScopeGeneration } from "@/lib/user-storage";

export const PdfAttachmentTarget = React.createContext<string | null>(null);
let pending: {
  files: Attachment[];
  generation: number;
  target: string;
} | null = null;
const listeners = new Set<() => void>();
export function queuePdfAttachments(files: Attachment[], target: string) {
  pending = { files, target, generation: storageScopeGeneration() };
  listeners.forEach((l) => l());
}
export function usePdfAttachments(fallbackTarget = "home") {
  const target = React.useContext(PdfAttachmentTarget) ?? fallbackTarget;
  return React.useSyncExternalStore(
    (listener) => {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    () =>
      pending?.target === target &&
      pending.generation === storageScopeGeneration()
        ? pending
        : null,
    () => null,
  );
}
export function takePdfAttachments(target: string) {
  if (pending?.target !== target) return [];
  const out = pending;
  pending = null;
  listeners.forEach((l) => l());
  return out?.generation === storageScopeGeneration() ? out.files : [];
}
