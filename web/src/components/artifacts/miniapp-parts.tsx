"use client";

/// アプリのエディターの、束ねた部品（ワークフロー・ビュー・スキル）の版（GAP-11・GAP-86）。
///
/// 部品は版で固定して束ねる（アプリの画面とボタンは固定した版のまま動く）。部品が改訂されていれば、新しいバージョンと、
/// 上げると変わるところ（サーバーの `POST /mini-apps/pin-updates` が決める）を出し、ボタンで最新に上げられる。上げた
/// 部品は、保存の前なら元のバージョンに戻せる。保存は下書きになり、公開するまで共有した人の画面は変わらない（D-16）。
/// 読めない部品（削除された・見る権限がない）は、変えなければそのまま保存できる（BUG-80）。
///
/// 差には、使ってよい操作と使う人に要る権限の増減も入る（GAP-11）。対象の名前はサーバーが返したもの（あなたに見える
/// 対象だけ）で言う。言い方は共有の画面と同じ（`access-wording.ts`）。

import * as React from "react";
import Link from "next/link";
import { ArrowUpCircle, Undo2, X } from "lucide-react";

import { Button } from "@/components/ui/button";
import { needWhat } from "@/lib/access-wording";
import type { PinDiff, PinUpdate } from "@/lib/artifact-api";
import { scopeLabel } from "@/lib/workflow-scopes";
import type { components } from "@/generated/api";

export type PartKind = "workflow" | "data_view" | "skill";

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

const NOUN: Record<PartKind, string> = { workflow: "ワークフロー", data_view: "ビュー", skill: "スキル" };

type InputHintType = components["schemas"]["InputHintType"];
type SkillPart = components["schemas"]["SkillPart"];

/// 入力項目が受ける値の種類の呼び名（用語集の値の種類）。
const TYPE_LABEL: Record<InputHintType, string> = {
  text: "テキスト",
  textarea: "テキスト（複数行）",
  number: "数値",
  choice: "選択肢",
  date: "日付",
  toggle: "はい／いいえ",
};

/// スキルの設定の呼び名（用語集のスキルの説明と同じ語）。
const SKILL_PART: Record<SkillPart, string> = {
  description: "スキルの説明",
  instructions: "AI への指示",
  knowledge_scope: "参照するフォルダーとファイル",
  allowed_tools: "使ってよいツール",
  model: "使うモデルの設定",
  few_shot: "お手本の対話",
  scripts: "スクリプト",
  references: "参考資料",
  command: "チャットで呼ぶコマンド",
};

/// 差が無いときの言い方（調べた範囲を言う・付録 C の P7）。ワークフローの要る権限は、前もって確かめられないブロック
/// （GAP-82）と実行のときに決まる対象を確かめないので、「確かめられた範囲では」と添える。ビューとスキルの要る権限は
/// 設定（テーブル・参照するフォルダーと参考資料）だけで決まるので言い切れる。
const NO_CHANGE: Record<PartKind, readonly string[]> = {
  workflow: ["入力項目と使ってよい操作は変わりません", "確かめられた範囲では、使う人に要る権限も変わりません"],
  data_view: ["表の設定と、使う人に要る権限は変わりません"],
  skill: ["スキルの設定と、使う人に要る権限は変わりません"],
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
    case "scope_added":
      return `使ってよい操作に「${scopeLabel(d.scope)}」が増えます`;
    case "scope_removed":
      return `使ってよい操作から「${scopeLabel(d.scope)}」がなくなります`;
    case "view_table":
      return "表示するテーブルが変わります";
    case "view_query":
      return "絞り込み・並べ替え・表示する列が変わります";
    case "view_display":
      return "列の並びや見出しなどの見た目が変わります";
    case "skill":
      return `${SKILL_PART[d.part]}が変わります`;
    case "access_added":
      return `使う人に、${needWhat(d.access)}権限が${d.conditional ? "場合によって" : ""}要るようになります`;
    case "access_always":
      return `使う人に、${needWhat(d.access)}権限が毎回要るようになります（いまは場合によって要ります）`;
    case "access_removed":
      return `使う人に、${needWhat(d.access)}権限が要らなくなります`;
  }
}

/// 使う人に要る権限が増えるか（増えるなら、公開の確認で相手ごとに確かめられることを添える）。
function needsMore(changes: readonly PinDiff[]): boolean {
  return changes.some((c) => c.kind === "access_added" || c.kind === "access_always");
}

/// 部品の中身を確かめる場所（差は中身を言わないので、開く先を言う）。
function WhereToLook({ kind, artifactId }: { kind: PartKind; artifactId: string }) {
  if (kind === "data_view") return null;
  const link = "underline underline-offset-2 hover:text-foreground";
  return kind === "workflow" ? (
    <p className="text-muted-foreground">
      処理の中身の変わり方は、
      <Link href={`/workflows/${artifactId}`} target="_blank" className={link}>
        ワークフローを開いて
      </Link>
      確かめてください。
    </p>
  ) : (
    <p className="text-muted-foreground">
      指示の中身の変わり方は、
      <Link href="/skills" target="_blank" className={link}>
        「スキル」の画面
      </Link>
      でスキルを開いて確かめてください。
    </p>
  );
}

/// 新しいバージョンがあれば、上げると変わるところと「vN にする」を出す（無ければ何も出さない）。
export function PinUpdateNote({
  kind,
  artifactId,
  version,
  check,
  onUpgrade,
  owner,
}: {
  kind: PartKind;
  artifactId: string;
  /// いま固定している版。
  version: number;
  check: PinCheck | undefined;
  onUpgrade: (artifactId: string, version: number) => void;
  /// エディターを開いた人がアプリの持ち主か（相手ごとの権限を確かめられるのは持ち主だけ・D-146）。
  owner: boolean;
}) {
  const update = check?.state === "readable" ? check.update : null;
  const latest = update?.latest_version ?? null;
  if (update == null || latest == null || latest <= version) return null;
  const changes = update.changes;
  return (
    <div className="mt-1.5 space-y-1.5 text-xs" data-testid="miniapp-pin-update">
      <p className="text-foreground/80">
        新しいバージョン v{latest} があります。
        {update.diff_unknown && changes.length === 0 ? "" : `v${latest} にすると、次のように変わります。`}
      </p>
      <ul className="list-disc space-y-0.5 pl-5 text-foreground/80" data-testid="miniapp-pin-changes">
        {changes.map((c, i) => (
          <li key={i} className="[overflow-wrap:anywhere]">
            {describePinDiff(c)}
          </li>
        ))}
        {update.diff_unknown ? (
          // 差を調べられなかった（固定している版が無い・本文を読めない）。「変わらない」とは言わない（付録 C の P7）。
          <li>
            {changes.length > 0
              ? "ほかに変わるところがあるかは、確かめられませんでした"
              : "変わるところを確かめられませんでした"}
          </li>
        ) : changes.length === 0 ? (
          NO_CHANGE[kind].map((line) => <li key={line}>{line}</li>)
        ) : null}
      </ul>
      {update.unchecked_blocks ? (
        // 共有の画面と同じ言い方（前もって確かめられないブロックがあれば、要る権限の増減を言い切らない・GAP-82）。
        <p className="text-amber-700 dark:text-amber-400" data-testid="miniapp-pin-unchecked">
          v{latest} には、前もって確かめられないブロックがあります。使う人に足りるかは、実行したときに分かります。
        </p>
      ) : null}
      {owner && needsMore(changes) ? (
        <p className="text-muted-foreground" data-testid="miniapp-pin-access-note">
          使う人に足りるかは、公開するときの確認で、共有している相手ごとに確かめられます。
        </p>
      ) : null}
      <WhereToLook kind={kind} artifactId={artifactId} />
      <Button type="button" size="sm" variant="outline" onClick={() => onUpgrade(artifactId, latest)}>
        <ArrowUpCircle className="size-4" aria-hidden />v{latest} にする
      </Button>
    </div>
  );
}

/// 保存してある版から上げた（変えた）ことと、元の版に戻すボタン（変えていなければ何も出さない）。
export function PinChangedNote({
  artifactId,
  version,
  savedVersion,
  onRevert,
}: {
  artifactId: string;
  version: number;
  /// 保存してある本文の版（同じ部品を束ねていなければ null）。
  savedVersion: number | null;
  onRevert: (artifactId: string, version: number) => void;
}) {
  if (savedVersion == null || savedVersion === version) return null;
  return (
    <div className="mt-1.5 flex flex-wrap items-center gap-x-2 gap-y-1 text-xs" data-testid="miniapp-pin-changed">
      <p className="min-w-0 flex-1 basis-56 text-foreground/80">
        v{savedVersion} から v{version} にしました。保存すると下書きになり、公開するまで共有した人の画面は変わりません。
      </p>
      <Button type="button" size="sm" variant="ghost" onClick={() => onRevert(artifactId, savedVersion)}>
        <Undo2 className="size-4" aria-hidden />v{savedVersion} に戻す
      </Button>
    </div>
  );
}

/// 名前で束ねる部品（ワークフロー・ビュー）の種類。スキルは選択欄で 1 つ選ぶ（エディターが上の 2 つを直に使う）。
type NamedKind = Exclude<PartKind, "skill">;

/// 束ねた部品の一覧（固定した版・新しいバージョン・上げる・元に戻す・外す）。
export function PartPins({
  kind,
  pins,
  saved,
  checks,
  onUpgrade,
  onRevert,
  onRemove,
  owner,
}: {
  kind: NamedKind;
  pins: readonly PartPin[];
  /// 保存してある本文の部品（上げた部品に、元のバージョンと「元に戻す」を出す）。
  saved: readonly PartPin[];
  checks: Readonly<Record<string, PinCheck>>;
  onUpgrade: (artifactId: string, version: number) => void;
  onRevert: (artifactId: string, version: number) => void;
  onRemove: (artifactId: string) => void;
  /// エディターを開いた人がアプリの持ち主か（[`PinUpdateNote`]）。
  owner: boolean;
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
          owner={owner}
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
  owner,
}: {
  kind: NamedKind;
  pin: PartPin;
  savedVersion: number | null;
  check: PinCheck | undefined;
  onUpgrade: (artifactId: string, version: number) => void;
  onRevert: (artifactId: string, version: number) => void;
  onRemove: (artifactId: string) => void;
  owner: boolean;
}) {
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
      <PinUpdateNote
        kind={kind}
        artifactId={pin.artifact_id}
        version={pin.version}
        check={check}
        onUpgrade={onUpgrade}
        owner={owner}
      />
      <PinChangedNote artifactId={pin.artifact_id} version={pin.version} savedVersion={savedVersion} onRevert={onRevert} />
      {/* ワークフローの読めない理由は、入力項目の突き合わせの注記が言う（同じことを 2 回言わない）。 */}
      {kind === "data_view" && check?.state === "unreadable" ? (
        <p role="note" className="mt-1.5 text-xs text-amber-700 dark:text-amber-400" data-testid="miniapp-pin-unreadable">
          {noun}「{pin.alias}」が見つかりません。削除されたか、見る権限がありません。変えなければ、このまま保存できます。
        </p>
      ) : null}
    </li>
  );
}
