"use client";

/// アプリのエディターの、束ねた部品（ワークフロー・ビュー）の一覧（GAP-11）。
///
/// 部品は版で固定して束ねる（アプリの画面とボタンは固定した版のまま動く）。部品が改訂されていれば、新しいバージョンと、
/// 上げると変わるところ（サーバーの `POST /mini-apps/pin-updates` が決める）を出し、ボタンで最新に上げられる。上げた
/// 部品は、保存の前なら元のバージョンに戻せる。保存は下書きになり、公開するまで共有した人の画面は変わらない（D-16）。
/// 読めない部品（削除された・見る権限がない）は、変えなければそのまま保存できる（BUG-80）。

import * as React from "react";
import Link from "next/link";
import { ArrowUpCircle, Undo2, X } from "lucide-react";

import { Button } from "@/components/ui/button";
import type { PinDiff, PinUpdate } from "@/lib/artifact-api";
import type { components } from "@/generated/api";

export type PartKind = "workflow" | "data_view";

/// 束ねた部品 1 つ（別名・部品の id・固定した版）。
export type PartPin = { alias: string; artifact_id: string; version: number };

/// 部品の版ごとの、新しいバージョンの確かめの結果。
export type PinCheck =
  | { state: "readable"; update: PinUpdate }
  /// 削除されたか、見る権限がない（存在を区別しない）。
  | { state: "unreadable" }
  /// 通信・サーバーの誤りで決められない（読めないと決めつけない）。
  | { state: "unknown" };

/// 確かめの結果を引く鍵（部品の種類・id・版）。
export function pinKey(kind: PartKind, pin: { artifact_id: string; version: number }): string {
  return `${kind}:${pin.artifact_id}@${pin.version}`;
}

const NOUN: Record<PartKind, string> = { workflow: "ワークフロー", data_view: "ビュー" };

type InputHintType = components["schemas"]["InputHintType"];

/// 入力項目が受ける値の種類の呼び名（用語集の値の種類）。
const TYPE_LABEL: Record<InputHintType, string> = {
  text: "テキスト",
  textarea: "テキスト（複数行）",
  number: "数値",
  choice: "選択肢",
  date: "日付",
  toggle: "はい／いいえ",
};

/// 入力項目の呼び方（表示名があれば表示名、英語の名前を添える・作成者向け）。
function inputName(key: string, label: string | null | undefined): string {
  return label && label !== key ? `「${label}」（${key}）` : `「${key}」`;
}

/// 最新のバージョンに上げると変わるところ 1 つの言い方。
export function describePinDiff(d: PinDiff): string {
  switch (d.kind) {
    case "input_added":
      return `入力項目${inputName(d.key, d.label)}が増えます${d.required ? "（必須）" : ""}`;
    case "input_removed":
      return `入力項目${inputName(d.key, d.label)}がなくなります`;
    case "input_required":
      return d.required
        ? `入力項目${inputName(d.key, d.label)}が必須になります`
        : `入力項目${inputName(d.key, d.label)}が必須でなくなります`;
    case "input_type":
      return `入力項目${inputName(d.key, d.label)}の種類が、${TYPE_LABEL[d.from]}から${TYPE_LABEL[d.to]}に変わります`;
    case "input_options":
      return `入力項目${inputName(d.key, d.label)}の選択肢が変わります`;
    case "view_table":
      return "表示するテーブルが変わります";
    case "view_query":
      return "絞り込み・並べ替え・表示する列が変わります";
    case "view_display":
      return "列の並びや見出しなどの見た目が変わります";
  }
}

/// 束ねた部品の一覧（固定した版・新しいバージョン・上げる・元に戻す・外す）。
export function PartPins({
  kind,
  pins,
  saved,
  checks,
  onUpgrade,
  onRevert,
  onRemove,
}: {
  kind: PartKind;
  pins: readonly PartPin[];
  /// 保存してある本文の部品（上げた部品に、元のバージョンと「元に戻す」を出す）。
  saved: readonly PartPin[];
  checks: Readonly<Record<string, PinCheck>>;
  onUpgrade: (artifactId: string, version: number) => void;
  onRevert: (artifactId: string, version: number) => void;
  onRemove: (artifactId: string) => void;
}) {
  if (pins.length === 0) return null;
  return (
    <ul className="space-y-1.5" data-testid={`miniapp-${kind}-pins`}>
      {pins.map((p) => (
        <PartPinRow
          key={p.artifact_id}
          kind={kind}
          pin={p}
          savedVersion={saved.find((s) => s.artifact_id === p.artifact_id)?.version ?? null}
          check={checks[pinKey(kind, p)]}
          onUpgrade={onUpgrade}
          onRevert={onRevert}
          onRemove={onRemove}
        />
      ))}
    </ul>
  );
}

function PartPinRow({
  kind,
  pin,
  savedVersion,
  check,
  onUpgrade,
  onRevert,
  onRemove,
}: {
  kind: PartKind;
  pin: PartPin;
  savedVersion: number | null;
  check: PinCheck | undefined;
  onUpgrade: (artifactId: string, version: number) => void;
  onRevert: (artifactId: string, version: number) => void;
  onRemove: (artifactId: string) => void;
}) {
  const update = check?.state === "readable" ? check.update : null;
  const latest = update?.latest_version ?? null;
  const newer = latest != null && latest > pin.version ? latest : null;
  const changed = savedVersion != null && savedVersion !== pin.version;
  const noun = NOUN[kind];
  return (
    <li
      className="rounded-lg border border-border/70 bg-secondary/30 px-3 py-2 text-sm"
      data-testid="miniapp-pin"
      data-alias={pin.alias}
    >
      <div className="flex items-center gap-2">
        <span className="min-w-0 flex-1 [overflow-wrap:anywhere]">
          <span className="font-medium">{pin.alias}</span>
          <span className="ml-1 text-xs text-muted-foreground">v{pin.version}</span>
        </span>
        <button
          type="button"
          aria-label={`${pin.alias} を外す`}
          onClick={() => onRemove(pin.artifact_id)}
          className="relative flex size-6 shrink-0 items-center justify-center rounded-full text-muted-foreground hover:text-destructive pointer-coarse:after:absolute pointer-coarse:after:-inset-3 pointer-coarse:after:content-['']"
        >
          <X className="size-3.5" aria-hidden />
        </button>
      </div>
      {newer != null ? (
        <div className="mt-1.5 space-y-1.5 text-xs" data-testid="miniapp-pin-update">
          <p className="text-foreground/80">
            新しいバージョン v{newer} があります。
            {update!.diff_unknown && update!.changes.length === 0 ? "" : `v${newer} にすると、次のように変わります。`}
          </p>
          <ul className="list-disc space-y-0.5 pl-5 text-foreground/80" data-testid="miniapp-pin-changes">
            {update!.changes.length > 0 ? (
              update!.changes.map((c, i) => (
                <li key={i} className="[overflow-wrap:anywhere]">
                  {describePinDiff(c)}
                </li>
              ))
            ) : update!.diff_unknown ? (
              // 差を調べられなかった（固定している版が無いなど）。「変わらない」とは言わない（付録 C の P7）。
              <li>変わるところを確かめられませんでした</li>
            ) : (
              <li>{kind === "workflow" ? "入力項目は変わりません" : "表の設定は変わりません"}</li>
            )}
          </ul>
          {kind === "workflow" ? (
            <p className="text-muted-foreground">
              処理の中身の変わり方は、
              <Link
                href={`/workflows/${pin.artifact_id}`}
                target="_blank"
                className="underline underline-offset-2 hover:text-foreground"
              >
                ワークフローを開いて
              </Link>
              確かめてください。
            </p>
          ) : null}
          <Button type="button" size="sm" variant="outline" onClick={() => onUpgrade(pin.artifact_id, newer)}>
            <ArrowUpCircle className="size-4" aria-hidden />
            v{newer} にする
          </Button>
        </div>
      ) : null}
      {changed ? (
        <div className="mt-1.5 flex flex-wrap items-center gap-x-2 gap-y-1 text-xs" data-testid="miniapp-pin-changed">
          <p className="min-w-0 flex-1 basis-56 text-foreground/80">
            v{savedVersion} から v{pin.version} にしました。保存すると下書きになり、公開するまで共有した人の画面は変わりません。
          </p>
          <Button type="button" size="sm" variant="ghost" onClick={() => onRevert(pin.artifact_id, savedVersion!)}>
            <Undo2 className="size-4" aria-hidden />
            v{savedVersion} に戻す
          </Button>
        </div>
      ) : null}
      {/* ワークフローの読めない理由は、入力項目の突き合わせの注記が言う（同じことを 2 回言わない）。 */}
      {kind === "data_view" && check?.state === "unreadable" ? (
        <p role="note" className="mt-1.5 text-xs text-amber-700 dark:text-amber-400" data-testid="miniapp-pin-unreadable">
          {noun}「{pin.alias}」が見つかりません。削除されたか、見る権限がありません。変えなければ、このまま保存できます。
        </p>
      ) : null}
    </li>
  );
}
