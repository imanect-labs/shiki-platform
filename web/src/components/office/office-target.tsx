"use client";

/// チャットの隣で開いている Office 文書（#731）。図の「Excel / Word に挿入」ボタンが入れる先。
///
/// Office の画面のチャット欄（[`OfficeChatPanel`]）だけが与える。ほかの画面（通常のチャット・
/// ノート）では null で、ボタンは出ない。

import * as React from "react";

import type { OfficeTargetKind } from "@/lib/office/chart-insert";

export type OfficeTarget = { fileId: string; kind: OfficeTargetKind };

const OfficeTargetContext = React.createContext<OfficeTarget | null>(null);

export function OfficeTargetProvider({ target, children }: { target: OfficeTarget | null; children: React.ReactNode }) {
  return <OfficeTargetContext.Provider value={target}>{children}</OfficeTargetContext.Provider>;
}

export function useOfficeTarget(): OfficeTarget | null {
  return React.useContext(OfficeTargetContext);
}
