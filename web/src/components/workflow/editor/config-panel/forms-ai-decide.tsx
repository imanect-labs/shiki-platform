"use client";

/// 「AI で判定」（ai.decide・FEAT-1・ir.md §7.11）の設定フォーム。
///
/// - 判定用のモデル: 組織が公開した判定用のモデルだけ（`modality=decision`・保存時の照合と同じ集合）。必須。
/// - 判定の材料（`state`）: 値の欄（差し込み・前の結果から）。**信頼しないデータ**として渡す。
/// - 質問: decide-questions.tsx（種類ごとの値域・リテラルだけ）。
/// - 振り分け（`route`）: しない／答えで分ける／確かなときだけ進める、使う質問、しきい値。
/// - 出口はサーバが設定から導いたもの（検証 API の `node_ports`）を見せる。TS に導出を書かない（H15）。
/// - 確からしさは較正されていない（AI の自己申告・得票率）ことを常に出す（D-52 の H4・PIT-80）。
///
/// 保存時の検証の誤りは、検証した時点の定義のキーで質問・選択肢を引いて、その欄の下に出す
/// （検証の後に並べ替えても取り違えない）。

import * as React from "react";
import { TriangleAlert } from "lucide-react";

import { Select, SelectContent, SelectItem, SelectValue } from "@/components/ui/select";
import type { NodeParamsByType } from "@/generated/workflow-catalog";
import type {
  DecideQuestion,
  DecideRoute,
  DecideRouteBy,
  Node as IrNode,
  ValidationError,
  ValueExpr,
} from "@/generated/workflow-ir";
import { portLabel } from "../node-card";
import { Field, FieldSelectTrigger, useFieldControl } from "./common";
import { DecideQuestions, kindLabel, type QuestionErrors } from "./decide-questions";
import type { FormProps } from "./forms-basic";
import { ModelField } from "./model-field";
import { paramsOf, patchParams } from "./params";
import { ValueExprInput } from "./value-expr-input";

type Params = Partial<NodeParamsByType["ai.decide"]>;
type RouteMode = "none" | DecideRouteBy;

/// しきい値の範囲（スライダ・ir.md §7.11 は 0.5〜1.0。1.0 では自己申告の確からしさがほぼ届かないので 0.99 まで）。
const MIN_CONFIDENCE = 0.5;
const MAX_CONFIDENCE = 0.99;
const DEFAULT_CONFIDENCE = 0.8;

const PARAMS_PREFIX = "params が不正です: ";
const strip = (m: string) => (m.startsWith(PARAMS_PREFIX) ? m.slice(PARAMS_PREFIX.length) : m);

/// 保存時の検証の誤りの振り分け（質問ごと・質問全体・振り分け）。
type DecideErrorView = {
  byQuestion: Map<number, QuestionErrors>;
  questions: string | null;
  route: Map<string, string>;
};

/// 誤りの path（`/params/questions/<i>/...`・`/params/route/...`）を今の並びに写す。添字は検証した時点の定義
/// （`checked`）のキーで引き、今の定義の同じキーの位置へ写す（無ければ添字のまま）。
function decideErrors(errors: ValidationError[], checked: IrNode | null, questions: DecideQuestion[]): DecideErrorView {
  const view: DecideErrorView = { byQuestion: new Map(), questions: null, route: new Map() };
  const checkedQuestions = ((checked?.params as Params | undefined)?.questions ?? []) as DecideQuestion[];
  for (const e of errors) {
    const path = e.path ?? "";
    const message = strip(e.message);
    if (path === "/params/questions") {
      view.questions ??= message;
      continue;
    }
    const route = /^\/params\/route\/([^/]+)/.exec(path);
    if (route) {
      if (!view.route.has(route[1])) view.route.set(route[1], message);
      continue;
    }
    const m = /^\/params\/questions\/(\d+)(?:\/(.*))?$/.exec(path);
    if (!m) continue;
    const at = Number(m[1]);
    const was = checkedQuestions[at];
    const now = was ? questions.findIndex((q) => q.key === was.key) : -1;
    const index = now >= 0 ? now : at;
    let field = m[2] ?? "key";
    const opt = /^options\/(\d+)\/(.+)$/.exec(field);
    if (opt && was) {
      const wasKey = was.options?.[Number(opt[1])]?.key;
      const j = wasKey !== undefined ? (questions[index]?.options ?? []).findIndex((o) => o.key === wasKey) : -1;
      if (j >= 0) field = `options/${j}/${opt[2]}`;
    }
    const map = view.byQuestion.get(index) ?? new Map<string, string>();
    if (!map.has(field)) map.set(field, message);
    view.byQuestion.set(index, map);
  }
  return view;
}

const NO_ERRORS: QuestionErrors = new Map();

export function AiDecideForm({
  node,
  dispatch,
  refCandidates,
  inMapRegion,
  fieldErrors,
  serverCheck,
  serverPorts,
}: FormProps) {
  const p = paramsOf<NodeParamsByType["ai.decide"]>(node) as Params;
  const questions = React.useMemo(() => (p.questions ?? []) as DecideQuestion[], [p.questions]);
  const route = p.route as DecideRoute | undefined;
  const errors = React.useMemo(
    () => decideErrors(serverCheck.errors, serverCheck.node, questions),
    [serverCheck, questions],
  );

  const patch = (next: Record<string, unknown>) => patchParams(dispatch, node, next);
  // 質問の一覧を書き戻す。振り分けに使う質問が無くなったら振り分けも外す（残すと保存できない）。
  const setQuestions = (next: DecideQuestion[]) => {
    const keep = route && next.some((q) => q.key === route.question);
    patch({ questions: next, ...(route && !keep ? { route: undefined } : {}) });
  };
  const onRenameKey = (_i: number, from: string, to: string, next: DecideQuestion[]) => {
    // 振り分けに使う質問のキーを変えたら、振り分けも同じ 1 手で付け替える。
    patch({ questions: next, ...(route && route.question === from ? { route: { ...route, question: to } } : {}) });
  };
  const onRenameOption = (i: number, from: string, to: string, next: DecideQuestion[]) => {
    const q = questions[i];
    // 答えで分けている質問の選択肢のキーは出口の名前なので、そこから出るエッジも付け替える（switch と同じ）。
    if (route && q && route.question === q.key && (route.by ?? "value") === "value" && from !== to) {
      dispatch({
        type: "rename_out_port",
        id: node.id,
        fromPort: from,
        toPort: to,
        params: { ...(node.params as Record<string, unknown>), questions: next },
        noMerge: true,
      });
      return;
    }
    patch({ questions: next });
  };

  return (
    <div className="space-y-4">
      <ModelField
        modality="decision"
        value={typeof p.model === "string" && p.model !== "" ? p.model : undefined}
        onChange={(model) => patch({ model: model ?? "" })}
        error={fieldErrors.get("model")}
      />
      <div className="space-y-1.5">
        <ValueExprInput
          label="判定の材料"
          text
          multiline
          value={p.state as ValueExpr | undefined}
          onChange={(v) => patch({ state: v })}
          refCandidates={refCandidates}
          inMapRegion={inMapRegion}
          placeholder="例: 問い合わせの本文（差し込むから選ぶ）"
          error={fieldErrors.get("state")}
        />
        <p className="text-[11px] leading-relaxed text-muted-foreground">
          {"AI はこの中に書かれた指示には従いません（信頼しないデータとして渡します）。文字・オブジェクト・配列で、JSON にして 32 KiB までです。"}
        </p>
      </div>
      <DecideQuestions
        questions={questions}
        onChange={setQuestions}
        onRenameKey={onRenameKey}
        onRenameOption={onRenameOption}
        errorsFor={(i) => errors.byQuestion.get(i) ?? NO_ERRORS}
        // 質問ごとに写せない誤り（一覧の件数・読み取りの誤り）は一覧の下に出す。
        generalError={errors.questions ?? (errors.byQuestion.size === 0 ? fieldErrors.get("questions") : null)}
      />
      <RouteSection
        questions={questions}
        route={route}
        onChange={(next) => patch({ route: next })}
        errors={errors.route}
        routeError={fieldErrors.get("route")}
        ports={serverPorts}
      />
    </div>
  );
}

function RouteSection({
  questions,
  route,
  onChange,
  errors,
  routeError,
  ports,
}: {
  questions: DecideQuestion[];
  route: DecideRoute | undefined;
  onChange: (next: DecideRoute | undefined) => void;
  errors: Map<string, string>;
  /// `route` の欄の誤り（読み取りの誤りなど・項目に写せないもの）。
  routeError?: string;
  /// サーバが導いた出口（まだ無ければ undefined）。
  ports: readonly string[] | undefined;
}) {
  const mode: RouteMode = route ? (route.by ?? "value") : "none";
  // 振り分けに使える質問（選択肢・はい/いいえ。段階では振り分けない）。
  const routable = questions.filter((q) => q.type === "choice" || q.type === "boolean");
  const setMode = (next: RouteMode) => {
    if (next === "none") return onChange(undefined);
    const question = route?.question && routable.some((q) => q.key === route.question) ? route.question : (routable[0]?.key ?? "");
    onChange({ question, by: next, min_confidence: route?.min_confidence ?? DEFAULT_CONFIDENCE });
  };
  const hint =
    mode === "none"
      ? "出口は 1 つです。答えは後ろのブロックで使えます（後ろに条件分岐を置いて分けることもできます）"
      : mode === "value"
        ? "答えの選択肢（はい／いいえ）ごとに出口が分かれます。確からしさがしきい値に届かなければ「要確認」へ進みます"
        : "確からしさがしきい値以上なら次へ、届かなければ「要確認」へ進みます";
  const unknownQuestion = route && route.question !== "" && !questions.some((q) => q.key === route.question);
  return (
    <div className="space-y-3 border-t pt-4" data-testid="decide-route">
      <Field label="振り分け" hint={hint} error={errors.get("by") ?? (errors.size === 0 ? routeError : undefined)}>
        <Select value={mode} onValueChange={(v) => setMode(v as RouteMode)}>
          <FieldSelectTrigger className="h-8" data-testid="decide-route-mode">
            <SelectValue />
          </FieldSelectTrigger>
          <SelectContent>
            <SelectItem value="none">振り分けない（出口は 1 つ）</SelectItem>
            <SelectItem value="value">答えで分ける</SelectItem>
            <SelectItem value="confidence">確かなときだけ進める</SelectItem>
          </SelectContent>
        </Select>
      </Field>
      {route ? (
        <>
          <Field
            label="振り分けに使う質問"
            hint={
              routable.length === 0
                ? "選択肢か、はい／いいえの質問を足すと振り分けられます（段階の質問では振り分けません）"
                : "選択肢か、はい／いいえの質問から選びます"
            }
            error={errors.get("question") ?? (unknownQuestion ? `質問「${route.question}」がありません` : undefined)}
          >
            <Select
              value={route.question || undefined}
              onValueChange={(question) => onChange({ ...route, question })}
              disabled={routable.length === 0}
            >
              <FieldSelectTrigger className="h-8" data-testid="decide-route-question">
                <SelectValue placeholder="質問を選ぶ" />
              </FieldSelectTrigger>
              <SelectContent>
                {routable.map((q) => (
                  <SelectItem key={q.key} value={q.key || "_"} disabled={!q.key}>
                    <span className="font-mono">{q.key}</span>
                    <span className="ml-1.5 text-muted-foreground">（{kindLabel(q.type)}）</span>
                  </SelectItem>
                ))}
                {route.question && !routable.some((q) => q.key === route.question) ? (
                  <SelectItem value={route.question}>{route.question}（使えない質問）</SelectItem>
                ) : null}
              </SelectContent>
            </Select>
          </Field>
          <Field
            label="しきい値（確からしさ）"
            hint={`確からしさが ${route.min_confidence.toFixed(2)} 以上なら答えに従い、届かなければ「要確認」へ進みます`}
            error={errors.get("min_confidence")}
          >
            <ThresholdInput value={route.min_confidence} onChange={(min_confidence) => onChange({ ...route, min_confidence })} />
          </Field>
        </>
      ) : null}
      <ExitsPreview ports={ports} hasRoute={route !== undefined} />
      <CalibrationNote />
    </div>
  );
}

/// しきい値のスライダ（0.50〜0.99・矢印キーで 0.01 ずつ・見出しを名前にする）。
function ThresholdInput({ value, onChange }: { value: number; onChange: (next: number) => void }) {
  const a11y = useFieldControl();
  const shown = Number.isFinite(value) ? value : DEFAULT_CONFIDENCE;
  return (
    <div className="flex items-center gap-3">
      <input
        type="range"
        min={MIN_CONFIDENCE}
        max={MAX_CONFIDENCE}
        step={0.01}
        value={Math.min(MAX_CONFIDENCE, Math.max(MIN_CONFIDENCE, shown))}
        onChange={(e) => onChange(Math.round(Number(e.target.value) * 100) / 100)}
        aria-valuetext={`${shown.toFixed(2)} 以上で進める`}
        data-testid="decide-threshold"
        {...a11y}
        className="h-6 min-w-0 flex-1 cursor-pointer accent-[var(--primary)] pointer-coarse:h-11"
      />
      <span className="w-10 shrink-0 text-right font-mono text-xs tabular-nums">{shown.toFixed(2)}</span>
    </div>
  );
}

/// 出口の一覧（サーバが導いたもの）。応答が来るまで・設定が契約に合わない間は、その旨を出す。
function ExitsPreview({ ports, hasRoute }: { ports: readonly string[] | undefined; hasRoute: boolean }) {
  return (
    <div className="space-y-1.5" data-testid="decide-exits">
      <p className="text-xs font-medium text-foreground">出口</p>
      {ports && ports.length > 0 ? (
        <ul className="flex flex-wrap gap-1.5">
          {ports.map((port) => (
            <li
              key={port}
              className="rounded-full border bg-background px-2 py-0.5 text-[11px]"
              data-port={port}
            >
              {port === "out" ? "次へ" : portLabel(port) || port}
              {portLabel(port) && port !== "out" && portLabel(port) !== port ? (
                <span className="ml-1 font-mono text-muted-foreground">{port}</span>
              ) : null}
            </li>
          ))}
        </ul>
      ) : (
        <p className="text-[11px] text-muted-foreground">出口は設定を直すと出ます（保存時の検証が決めます）</p>
      )}
      {hasRoute ? (
        <p className="text-[11px] leading-relaxed text-muted-foreground">
          {"「要確認」の出口は「承認を待つ」ブロックへつなぐと、人が確かめてから進められます（AI の判定だけで承認・却下を決めない）。"}
        </p>
      ) : null}
    </div>
  );
}

/// 確からしさが較正されていないことの注記（H4・常に出す）。
function CalibrationNote() {
  return (
    <div
      role="note"
      className="flex items-start gap-1.5 rounded-md border p-2 text-[11px] leading-relaxed"
      style={{
        borderColor: "color-mix(in oklab, var(--season-autumn) 45%, transparent)",
        backgroundColor: "color-mix(in oklab, var(--season-autumn) 8%, transparent)",
      }}
      data-testid="decide-calibration"
    >
      <TriangleAlert className="mt-0.5 size-3.5 shrink-0 text-[var(--season-autumn)]" aria-hidden />
      <span>
        <span className="font-medium">確からしさ: 較正なし。</span>
        AI の自己申告（または多数決の得票率）で、高めに出がちです。しきい値は実際の答えで確かめてから決めてください。
      </span>
    </div>
  );
}
