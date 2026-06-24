"use client";

// 軽量トーストストア（外部状態 + useSyncExternalStore）。
// どこからでも `toast({ title, description })` で呼べるシングルトン。
import * as React from "react";

type ToastVariant = "default" | "destructive";

export type ToastItem = {
  id: string;
  title?: React.ReactNode;
  description?: React.ReactNode;
  variant?: ToastVariant;
  duration?: number;
  open: boolean;
};

type ToastInput = Omit<ToastItem, "id" | "open">;

const TOAST_LIMIT = 4;
const TOAST_REMOVE_DELAY = 400; // 閉アニメーション後に DOM から除去するまでの猶予

let counter = 0;
function nextId(): string {
  counter = (counter + 1) % Number.MAX_SAFE_INTEGER;
  return String(counter);
}

let memoryState: ToastItem[] = [];
const listeners = new Set<() => void>();
const removeTimers = new Map<string, ReturnType<typeof setTimeout>>();

function emit() {
  for (const listener of listeners) listener();
}

function scheduleRemove(id: string) {
  if (removeTimers.has(id)) return;
  const timer = setTimeout(() => {
    removeTimers.delete(id);
    memoryState = memoryState.filter((t) => t.id !== id);
    emit();
  }, TOAST_REMOVE_DELAY);
  removeTimers.set(id, timer);
}

/// トーストを表示する。返り値で個別に閉じられる。
export function toast(input: ToastInput) {
  const id = nextId();
  const item: ToastItem = { ...input, id, open: true };
  memoryState = [item, ...memoryState].slice(0, TOAST_LIMIT);
  emit();

  const dismiss = () => {
    memoryState = memoryState.map((t) => (t.id === id ? { ...t, open: false } : t));
    emit();
    scheduleRemove(id);
  };

  return { id, dismiss };
}

/// open=false への遷移（Radix の onOpenChange）を受けて除去予約する。
export function setToastOpen(id: string, open: boolean) {
  memoryState = memoryState.map((t) => (t.id === id ? { ...t, open } : t));
  emit();
  if (!open) scheduleRemove(id);
}

const getSnapshot = () => memoryState;
const getServerSnapshot = () => memoryState;

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/// 現在のトースト配列を購読する（Toaster 用）。
export function useToast(): ToastItem[] {
  return React.useSyncExternalStore(subscribe, getSnapshot, getServerSnapshot);
}
