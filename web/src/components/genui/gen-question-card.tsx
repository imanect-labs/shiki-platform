"use client";

/// 質問カード（Claude Code の AskUserQuestion 相当・PR4）。
///
/// AI がユーザーへ質問を提示し回答を集める。**1 問ずつステップ表示**（別ページのような体験）し、
/// 各選択肢は**ラベル＋説明**のカードで示す。選択肢に無い回答は「その他」の自由記述（テキスト
/// エリア・長文可）で受ける。回答は宣言済みアクション（chat.submit）へまとめて送信され次ターンの
/// 発話になる。配色/モーションはアプリのデザイン言語（season アクセント・elevation/duration/ease
/// トークン・motion-primitives）に準拠する。

import * as React from "react";

import { ArrowLeft, ArrowRight, Check, MessagesSquare, PencilLine } from "lucide-react";
import { AnimatePresence, motion } from "motion/react";

import type { QuestionCardProps, QuestionItem } from "@/generated/gui-spec";
import { Button } from "@/components/ui/button";
import { DURATION_NORMAL, EASE_STANDARD, PRESSABLE } from "@/components/ui/motion-primitives";
import { currentSeasonIndex, seasonAccentStyle } from "@/lib/season";
import { cn } from "@/lib/utils";
import { useGenUiAction } from "./action-context";
import { ActionResultNote, describeActionError } from "./action-result";

const OTHER = "__other__";

/// 1 問の回答状態。options 質問は選択ラベル集合＋その他自由記述、自由記述質問は text。
type Answer = { selected: string[]; other: string; text: string };

function emptyAnswer(): Answer {
  return { selected: [], other: "", text: "" };
}

/// 質問の回答を「見出し（無ければ質問文）: 回答」の可読テキストへ整形するためのキー。
function answerKey(q: QuestionItem): string {
  return q.header?.trim() || q.question;
}

/// 1 問分の回答を送信用の文字列にする（未回答は空文字＝バックエンドが除外）。
function answerValue(q: QuestionItem, a: Answer): string {
  if ((q.options ?? []).length === 0) return a.text.trim();
  const parts = a.selected.filter((s) => s !== OTHER);
  if (a.selected.includes(OTHER) && a.other.trim()) parts.push(a.other.trim());
  return parts.join("、");
}

export function GenUiQuestionCard({ card }: { card: QuestionCardProps }) {
  const { dispatch, onActionCompleted } = useGenUiAction();
  const questions = React.useMemo(() => card.questions ?? [], [card.questions]);
  const total = questions.length;

  const [answers, setAnswers] = React.useState<Record<string, Answer>>({});
  const [step, setStep] = React.useState(0);
  const [dir, setDir] = React.useState(1); // 進む=1 / 戻る=-1（トランジション方向）
  const [busy, setBusy] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);
  const [done, setDone] = React.useState(false);

  if (total === 0) return null;
  const q = questions[Math.min(step, total - 1)];
  const options = q.options ?? []; // フィクスチャ等で省略され得る（防御的に空扱い）。
  const a = answers[q.id] ?? emptyAnswer();
  const isLast = step >= total - 1;

  const update = (patch: Partial<Answer>) =>
    setAnswers((prev) => ({ ...prev, [q.id]: { ...(prev[q.id] ?? emptyAnswer()), ...patch } }));

  const toggle = (label: string) => {
    const has = a.selected.includes(label);
    if (q.multi_select) {
      update({ selected: has ? a.selected.filter((s) => s !== label) : [...a.selected, label] });
    } else {
      // 単一選択: その選択肢だけにする（同じものを再タップで解除）。
      update({ selected: has ? [] : [label] });
    }
  };

  const go = (next: number) => {
    setDir(next > step ? 1 : -1);
    setStep(Math.max(0, Math.min(total - 1, next)));
  };

  const onSubmit = async () => {
    if (busy || done) return;
    setBusy(true);
    setError(null);
    // 回答は「質問の見出し（無ければ質問文）: 回答」で送る。__proto__ 等でも own property に
    // なるよう null プロトタイプにし、キー衝突時は id を添えて一意化する。
    const payload = Object.create(null) as Record<string, string>;
    const has = (k: string) => Object.prototype.hasOwnProperty.call(payload, k);
    for (const item of questions) {
      const base = answerKey(item);
      let key = base;
      if (has(key)) {
        key = `${base}（${item.id}）`;
        for (let n = 2; has(key); n++) key = `${base}（${item.id}）-${n}`;
      }
      payload[key] = answerValue(item, answers[item.id] ?? emptyAnswer());
    }
    try {
      const result = await dispatch(card.submit.action, payload);
      setDone(true);
      onActionCompleted?.(result);
    } catch (err) {
      setError(describeActionError(err));
    } finally {
      setBusy(false);
    }
  };

  const seasonIdx = currentSeasonIndex();

  return (
    <div style={seasonAccentStyle(seasonIdx)} className="min-w-0">
      {/* ヘッダ: AI からの問いかけ＋進捗。 */}
      <div className="flex items-center gap-2.5">
        <span
          className="grid size-8 shrink-0 place-items-center rounded-lg"
          style={{
            backgroundColor: "color-mix(in oklab, var(--season) 16%, transparent)",
            color: "var(--season)",
          }}
          aria-hidden
        >
          <MessagesSquare className="size-4" />
        </span>
        <div className="min-w-0 flex-1">
          <h3 className="truncate text-[13px] font-semibold tracking-wide text-foreground">
            {card.title || "AI からの質問"}
          </h3>
        </div>
        <span className="shrink-0 text-[11px] font-medium tabular-nums text-muted-foreground">
          {step + 1}
          <span className="text-muted-foreground/50"> / {total}</span>
        </span>
      </div>

      {/* 進捗バー（season 色・幅をトークン持続でトゥイーン）。 */}
      <div className="mt-2 h-1 w-full overflow-hidden rounded-full bg-secondary">
        <div
          className="h-full rounded-full transition-[width] duration-[var(--duration-normal)] ease-[var(--ease-standard)]"
          style={{ width: `${((step + 1) / total) * 100}%`, backgroundColor: "var(--season)" }}
        />
      </div>

      {card.intro && step === 0 ? (
        <p className="mt-3 whitespace-pre-wrap text-xs leading-relaxed text-muted-foreground">
          {card.intro}
        </p>
      ) : null}

      {/* 質問本体（1 問ずつ・左右スライドで切り替え）。 */}
      <div className="relative mt-3 overflow-hidden">
        <AnimatePresence mode="wait" custom={dir} initial={false}>
          <motion.div
            key={q.id}
            custom={dir}
            initial={{ opacity: 0, x: dir * 20 }}
            animate={{ opacity: 1, x: 0 }}
            exit={{ opacity: 0, x: dir * -20 }}
            transition={{ duration: DURATION_NORMAL, ease: EASE_STANDARD }}
          >
            {q.header ? (
              <span
                className="mb-2 inline-block rounded-full px-2 py-0.5 text-[11px] font-medium"
                style={{
                  backgroundColor: "color-mix(in oklab, var(--season) 14%, transparent)",
                  color: "var(--season)",
                }}
              >
                {q.header}
              </span>
            ) : null}
            <p className="text-[15px] font-medium leading-relaxed text-foreground">{q.question}</p>

            {options.length > 0 ? (
              <div className="mt-3 flex flex-col gap-2" role={q.multi_select ? "group" : "radiogroup"}>
                {options.map((opt) => (
                  <OptionCard
                    key={opt.label}
                    label={opt.label}
                    description={opt.description ?? null}
                    multi={q.multi_select}
                    selected={a.selected.includes(opt.label)}
                    disabled={busy || done}
                    onSelect={() => toggle(opt.label)}
                  />
                ))}
                {q.allow_other ? (
                  <OptionCard
                    label="その他"
                    description="選択肢にない場合は自由に入力してください"
                    icon={<PencilLine className="size-4" />}
                    multi={q.multi_select}
                    selected={a.selected.includes(OTHER)}
                    disabled={busy || done}
                    onSelect={() => toggle(OTHER)}
                  />
                ) : null}
                {q.allow_other && a.selected.includes(OTHER) ? (
                  <textarea
                    value={a.other}
                    onChange={(e) => update({ other: e.target.value })}
                    placeholder={q.placeholder ?? "自由に入力してください"}
                    rows={2}
                    disabled={busy || done}
                    className="mt-0.5 w-full resize-y rounded-lg border border-input bg-background px-3 py-2 text-sm text-foreground shadow-xs focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-50"
                  />
                ) : null}
              </div>
            ) : (
              <textarea
                value={a.text}
                onChange={(e) => update({ text: e.target.value })}
                placeholder={q.placeholder ?? "回答を入力してください"}
                rows={4}
                disabled={busy || done}
                className="mt-3 w-full resize-y rounded-lg border border-input bg-background px-3 py-2.5 text-sm leading-relaxed text-foreground shadow-xs focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-50"
              />
            )}
          </motion.div>
        </AnimatePresence>
      </div>

      {/* フッタ: 戻る / 次へ・回答する。 */}
      <div className="mt-4 flex items-center gap-3">
        <button
          type="button"
          onClick={() => go(step - 1)}
          disabled={step === 0 || busy || done}
          className={cn(
            "inline-flex items-center gap-1 rounded-lg px-2.5 py-1.5 text-xs font-medium text-muted-foreground",
            "transition-colors hover:bg-secondary hover:text-foreground disabled:pointer-events-none disabled:opacity-40",
            PRESSABLE,
          )}
        >
          <ArrowLeft className="size-3.5" aria-hidden />
          戻る
        </button>
        <div className="flex-1" />
        {done ? (
          <span className="inline-flex items-center gap-1 text-xs text-primary">
            <Check className="size-3.5" aria-hidden />
            回答を送信しました
          </span>
        ) : isLast ? (
          <Button type="button" size="sm" onClick={onSubmit} disabled={busy} className={PRESSABLE}>
            {card.submit_label || "回答する"}
          </Button>
        ) : (
          <Button type="button" size="sm" onClick={() => go(step + 1)} className={PRESSABLE}>
            次へ
            <ArrowRight className="size-3.5" aria-hidden />
          </Button>
        )}
      </div>
      <ActionResultNote error={error} />
    </div>
  );
}

/// 選択肢 1 枚（ラベル＋説明・node-card と同じ elevation/ring イディオム）。
function OptionCard({
  label,
  description,
  multi,
  selected,
  disabled,
  icon,
  onSelect,
}: {
  label: string;
  description: string | null;
  multi: boolean;
  selected: boolean;
  disabled: boolean;
  icon?: React.ReactNode;
  onSelect: () => void;
}) {
  return (
    <button
      type="button"
      role={multi ? "checkbox" : "radio"}
      aria-checked={selected}
      onClick={onSelect}
      disabled={disabled}
      data-testid="genui-question-option"
      className={cn(
        "group/opt flex w-full items-start gap-3 rounded-xl border bg-card px-3.5 py-2.5 text-left shadow-sm",
        "transition-[transform,box-shadow,border-color,background-color] duration-[var(--duration-fast)] ease-[var(--ease-standard)]",
        "disabled:pointer-events-none disabled:opacity-60",
        selected
          ? "border-primary bg-[color-mix(in_oklab,var(--primary)_5%,var(--card))] shadow-md ring-1 ring-primary"
          : "hover:-translate-y-px hover:border-foreground/15 hover:shadow-md",
      )}
    >
      {/* 選択インジケータ（単一=丸／複数=角丸）。 */}
      <span
        className={cn(
          "mt-0.5 grid size-5 shrink-0 place-items-center border transition-colors",
          multi ? "rounded-md" : "rounded-full",
          selected ? "border-primary bg-primary text-primary-foreground" : "border-input bg-background",
        )}
        aria-hidden
      >
        {selected ? <Check className="size-3.5" /> : null}
      </span>
      <span className="min-w-0 flex-1">
        <span className="flex items-center gap-1.5 text-sm font-medium text-foreground">
          {icon ? (
            <span className="text-muted-foreground group-hover/opt:text-foreground" aria-hidden>
              {icon}
            </span>
          ) : null}
          {label}
        </span>
        {description ? (
          <span className="mt-0.5 block text-xs leading-relaxed text-muted-foreground">
            {description}
          </span>
        ) : null}
      </span>
    </button>
  );
}
