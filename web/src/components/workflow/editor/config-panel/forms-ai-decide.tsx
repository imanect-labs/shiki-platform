"use client";

/// 「AI で判定」（ai.decide・FEAT-1・ir.md §7.11）の設定フォーム。
///
/// - 判定用のモデル: 組織が公開した判定用のモデルだけ（`modality=decision`・保存時の照合と同じ集合）。必須。
/// - 判定の材料（`state`）: 値の欄（差し込み・前の結果から）。**信頼しないデータ**として渡す。
/// - 質問: decide-questions.tsx（種類ごとの値域・リテラルだけ）。
/// - 振り分け（`route`）: しない／答えで分ける／確かなときだけ進める、使う質問、しきい値。
/// - 出口はサーバが設定から導いたもの（検証 API の `node_ports`）を見せる。TS に導出を書かない（H15）。
/// - 確からしさは較正されていない（AI の自己申告・得票率）ことを常に出す（D-52 の H4・PIT-80）。
/// - 答えの使い回し（step 属性 `cache`・決定キャッシュ・T10）は既定で切る。期間の決まりはノードカタログの
///   `step_cache`（Rust の単一定義）から読み、範囲の外の候補は出さない。
///
/// 保存時の検証の誤りは、検証した時点の定義のキーで質問・選択肢を引いて、その欄の下に出す
/// （検証の後に並べ替えても取り違えない）。

import * as React from "react";
import { TriangleAlert } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectValue } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { cn } from "@/lib/utils";
import { NODE_CATALOG, type NodeParamsByType } from "@/generated/workflow-catalog";
import type {
  DecideQuestion,
  DecideRoute,
  DecideRouteBy,
  Node as IrNode,
  StepCache,
  StepCacheLimits,
  ValidationError,
  ValueExpr,
} from "@/generated/workflow-ir";
import { nodePortLabel } from "../../port-labels";
import { BLANK_REQUIRED_MESSAGE, Field, FieldSelectTrigger, useFieldControl } from "./common";
import { DecideQuestions, kindLabel, type QuestionErrors } from "./decide-questions";
import { planOptionRename, planRouteRename, renameShape } from "./decide-renames";
import type { FormProps } from "./forms-basic";
import { ANY_SHAPES } from "./value-shape";
import { ModelField } from "./model-field";
import { paramsOf, patchParams } from "./params";
import { ValueExprInput } from "./value-expr-input";

type Params = Partial<NodeParamsByType["ai.decide"]>;
type RouteMode = "none" | DecideRouteBy;

/// しきい値の範囲（ir.md §7.11 の 0.5〜1.0）。既定は置かない（版ごとに実測で決める・PIT-80・D-88）。
const MIN_CONFIDENCE = 0.5;
const MAX_CONFIDENCE = 1;
/// 「0.80 から始める」で入れる仮の値（既定ではない・押した人が選んだと分かるようにボタンで入れる・D-88）。
const STARTING_CONFIDENCE = 0.8;

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
  serverPortsFresh = true,
  edgePorts = [],
}: FormProps) {
  const p = paramsOf<NodeParamsByType["ai.decide"]>(node) as Params;
  const questions = React.useMemo(() => (p.questions ?? []) as DecideQuestion[], [p.questions]);
  const route = p.route as DecideRoute | undefined;
  const thresholdIsBlank = route !== undefined && typeof route.min_confidence !== "number";
  const routeBlank = fieldErrors.get("route") === BLANK_REQUIRED_MESSAGE;
  const errors = React.useMemo(
    () => decideErrors(serverCheck.errors, serverCheck.node, questions),
    [serverCheck, questions],
  );

  const patch = (next: Record<string, unknown>) => patchParams(dispatch, node, next);
  // 打ち変えの途中で、ほかの選択肢・質問と同じキーを通っている間の付け替えの記憶（decide-renames.ts）。
  // 選択肢は「質問の位置:選択肢の位置」→ その選択肢のエッジが残っている出口、振り分けは持ち主の質問の位置。
  const heldPorts = React.useRef(new Map<string, string>());
  const heldRoute = React.useRef<number | undefined>(undefined);
  // 質問の一覧を書き戻す。振り分けに使う質問が無くなったら振り分けも外す（残すと保存できない）。
  const setQuestions = (next: DecideQuestion[]) => {
    // 並べ替え・追加・削除・種類の変更で位置とキーが変わったら、付け替えの記憶は捨てる（別の行を指さない）。
    if (renameShape(next) !== renameShape(questions)) {
      heldPorts.current.clear();
      heldRoute.current = undefined;
    }
    const keep = route && next.some((q) => q.key === route.question);
    patch({ questions: next, ...(route && !keep ? { route: undefined } : {}) });
  };
  const onRenameKey = (i: number, _from: string, to: string, next: DecideQuestion[]) => {
    // 振り分けに使う質問のキーを変えたら、振り分けも同じ 1 手で付け替える。ほかの質問のキーを打ち変えて振り分けの
    // 質問のキーを通っても、振り分けは動かさない。
    const plan = route
      ? planRouteRename({ prevKeys: questions.map((q) => q.key), index: i, to, route: route.question, held: heldRoute.current })
      : null;
    heldRoute.current = plan?.held;
    patch({ questions: next, ...(route && plan?.question != null ? { route: { ...route, question: plan.question } } : {}) });
  };
  const onRenameOption = (i: number, from: string, to: string, next: DecideQuestion[]) => {
    const q = questions[i];
    // 答えで分けている質問の選択肢のキーは出口の名前なので、そこから出るエッジも付け替える（switch と同じ）。
    // 出口の名前＝選択肢のキーという規則だけを写している（集合は node_ports・decide-renames.ts）。
    const byValue = route && q && route.question === q.key && (route.by ?? "value") === "value";
    const prevKeys = (q?.options ?? []).map((o) => o.key);
    const index = (next[i]?.options ?? []).findIndex((o, j) => o.key !== prevKeys[j]);
    if (!byValue || from === to || index < 0) {
      patch({ questions: next });
      return;
    }
    const slot = `${i}:${index}`;
    const plan = planOptionRename({ prevKeys, index, to, held: heldPorts.current.get(slot), edgePorts });
    if (plan.held === undefined) heldPorts.current.delete(slot);
    else heldPorts.current.set(slot, plan.held);
    if (!plan.move) {
      patch({ questions: next });
      return;
    }
    dispatch({
      type: "rename_out_port",
      id: node.id,
      fromPort: plan.move.from,
      toPort: plan.move.to,
      params: { ...(node.params as Record<string, unknown>), questions: next },
      noMerge: true,
    });
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
          shapes={ANY_SHAPES}
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
        node={node}
        questions={questions}
        route={route}
        onChange={(next) => patch({ route: next })}
        errors={errors.route}
        // しきい値が空の保存時の注意（`/params/route/min_confidence`・欄の空の知らせ）は、振り分けではなくしきい値の
        // 欄の下に出す。入れた直後に検証が追いつくまで残る知らせも、振り分けの下には出さない。
        routeError={routeBlank ? undefined : fieldErrors.get("route")}
        blankThreshold={thresholdIsBlank && routeBlank}
        ports={serverPorts}
        portsState={
          serverPortsFresh
            ? "fresh"
            : (serverCheck.node?.params as unknown) === node.params
              ? "unresolved"
              : "pending"
        }
      />
      {CACHE_LIMITS ? (
        <CacheSection
          limits={CACHE_LIMITS}
          value={node.cache ?? null}
          onChange={(cache) => dispatch({ type: "update_node", id: node.id, patch: { cache } as Partial<IrNode> })}
        />
      ) : null}
    </div>
  );
}

/// 決定キャッシュの期間の決まり（ノードカタログの `step_cache`・Rust の単一定義）。
const CACHE_LIMITS: StepCacheLimits | null =
  NODE_CATALOG.find((e) => e.type === "ai.decide")?.step_cache ?? null;

/// 期間の候補（秒）。決まりの範囲の内のものだけ出す。
const CACHE_TTL_PRESETS = [3600, 86_400, 7 * 86_400, 30 * 86_400];

/// 期間を人が読める単位で（日・時間・分・秒の大きい方から割り切れるもの）。
function ttlLabel(secs: number): string {
  if (secs % 86_400 === 0) return `${secs / 86_400} 日`;
  if (secs % 3600 === 0) return `${secs / 3600} 時間`;
  if (secs % 60 === 0) return `${secs / 60} 分`;
  return `${secs} 秒`;
}

/// 答えの使い回し（決定キャッシュ・step 属性 `cache`）。既定は切る（D-52 の H6）。
function CacheSection({
  limits,
  value,
  onChange,
}: {
  limits: StepCacheLimits;
  value: StepCache | null;
  onChange: (next: StepCache | null) => void;
}) {
  const on = value !== null;
  const ttl = value?.ttl_sec ?? limits.default_ttl_sec;
  const presets = CACHE_TTL_PRESETS.filter((s) => s >= limits.min_ttl_sec && s <= limits.max_ttl_sec);
  // 保存済みの値が候補に無い（AI・JSON で書いた）ときは、そのまま選べるように足す。
  const options = presets.includes(ttl) ? presets : [...presets, ttl].sort((a, b) => a - b);
  return (
    <div className="space-y-3 border-t pt-4" data-testid="decide-cache">
      <label className="flex items-center justify-between gap-3">
        <span className="text-xs font-medium text-foreground">同じ内容なら答えを使い回す</span>
        <Switch
          aria-label="同じ内容なら答えを使い回す"
          checked={on}
          onCheckedChange={(checked) => onChange(checked ? { ttl_sec: limits.default_ttl_sec } : null)}
          data-testid="decide-cache-switch"
        />
      </label>
      {on ? (
        <Field
          label="使い回す期間"
          hint={`${ttlLabel(limits.min_ttl_sec)}〜${ttlLabel(limits.max_ttl_sec)}。過ぎたら、もう一度 AI に判定させます`}
        >
          <Select value={String(ttl)} onValueChange={(v) => onChange({ ttl_sec: Number(v) })}>
            <FieldSelectTrigger className="h-8" data-testid="decide-cache-ttl">
              <SelectValue />
            </FieldSelectTrigger>
            <SelectContent>
              {options.map((s) => (
                <SelectItem key={s} value={String(s)}>
                  {ttlLabel(s)}
                  {presets.includes(s) ? null : <span className="ml-1.5 text-muted-foreground">（設定済み）</span>}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </Field>
      ) : null}
      <p className="text-[11px] leading-relaxed text-muted-foreground">
        {on
          ? "このワークフローの中で、質問・判定の材料・判定用のモデルの版がすべて同じなら、前の答えをそのまま使います（AI を呼ばないので費用はかかりません。組織のモデルの許可は毎回確かめます）。答えは期間のあいだサーバに残ります（材料そのものは残しません）。"
          : "使い回さず、毎回 AI に判定させます。同じ問い合わせを何度も流すフローで費用を抑えたいときに使います。"}
      </p>
    </div>
  );
}

function RouteSection({
  node,
  questions,
  route,
  onChange,
  errors,
  routeError,
  blankThreshold,
  ports,
  portsState,
}: {
  node: IrNode;
  questions: DecideQuestion[];
  route: DecideRoute | undefined;
  onChange: (next: DecideRoute | undefined) => void;
  errors: Map<string, string>;
  /// `route` の欄の誤り（読み取りの誤りなど・項目に写せないもの）。
  routeError?: string;
  /// しきい値が空だという保存時の注意があるか（欄の下に「入れてください」を出す・D-88）。
  blankThreshold: boolean;
  /// サーバが導いた出口（まだ無ければ undefined）。
  ports: readonly string[] | undefined;
  /// 出口が今の設定のものか（`pending` は検証の応答待ち・`unresolved` は今の設定では導けなかった）。
  portsState: ExitsState;
}) {
  const mode: RouteMode = route ? (route.by ?? "value") : "none";
  // 振り分けに使える質問（選択肢・はい/いいえ。段階では振り分けない）。
  const routable = questions.filter((q) => q.type === "choice" || q.type === "boolean");
  const setMode = (next: RouteMode) => {
    if (next === "none") return onChange(undefined);
    const question = route?.question && routable.some((q) => q.key === route.question) ? route.question : (routable[0]?.key ?? "");
    // しきい値は入れない（既定を置かない・「0.80 から始める」で入れる・D-88）。前に入れた値は残す。
    onChange({
      question,
      by: next,
      ...(route?.min_confidence !== undefined && route.min_confidence !== null ? { min_confidence: route.min_confidence } : {}),
    });
  };
  const hint =
    mode === "none"
      ? "出口は 1 つです。答えは後ろのブロックで使えます（後ろに条件分岐を置いて分けることもできます）"
      : mode === "value"
        ? "答えの選択肢（はい／いいえ）ごとに出口が分かれます。確からしさがしきい値に届かなければ「要確認」へ進みます"
        : "確からしさがしきい値以上なら次へ、届かなければ「要確認」へ進みます";
  const unknownQuestion = route && route.question !== "" && !questions.some((q) => q.key === route.question);
  const threshold = typeof route?.min_confidence === "number" ? route.min_confidence : undefined;
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
            label="しきい値（AI の自信）"
            hint={thresholdHint(mode, threshold)}
            error={
              errors.get("min_confidence") ??
              thresholdRangeError(threshold) ??
              (threshold === undefined && blankThreshold ? "「しきい値（AI の自信）」を入れてください。" : undefined)
            }
          >
            <ThresholdInput
              value={threshold}
              onChange={(min_confidence) => {
                // 空にしたら項目ごと外す（空は保存でき、実行の前に止まる・D-88）。
                const rest: DecideRoute = { question: route.question, ...(route.by ? { by: route.by } : {}) };
                onChange(min_confidence === undefined ? rest : { ...rest, min_confidence });
              }}
            />
          </Field>
        </>
      ) : null}
      <ExitsPreview node={node} ports={ports} state={portsState} hasRoute={route !== undefined} />
      <CalibrationNote />
    </div>
  );
}

/// しきい値の注記（分け方ごとに進む出口を言う・数は 0〜1 を主に % を添える・付録 D の N3.2.2）。
function thresholdHint(mode: RouteMode, value: number | undefined): string {
  const at = value === undefined ? "しきい値" : ` ${value.toFixed(2)}（${Math.round(value * 100)}%）`;
  const pass = mode === "confidence" ? "「次へ」へ進み" : "答えに従い";
  const head = `AI の自信が${at}以上なら${pass}、届かないときは「要確認」へ進みます。`;
  return value === undefined ? `${head}決まった値はありません。空のままでは実行できません。` : head;
}

/// しきい値の範囲の誤り（欄の下・保存時の検証と同じ範囲）。
function thresholdRangeError(value: number | undefined): string | undefined {
  if (value === undefined || (value >= MIN_CONFIDENCE && value <= MAX_CONFIDENCE)) return undefined;
  return "しきい値は 0.50〜1.00 にしてください。";
}

/// しきい値の欄（0.50〜1.00 を数で入れる）。空のときは「0.80 から始める」で仮の値を入れられる（既定は置かない・
/// D-88・PIT-80）。
function ThresholdInput({ value, onChange }: { value: number | undefined; onChange: (next: number | undefined) => void }) {
  const a11y = useFieldControl();
  // 打っている途中の文字（「0.」など）を保つ。値が外から変わったら合わせる。
  const [text, setText] = React.useState(value === undefined ? "" : String(value));
  React.useEffect(() => {
    setText((cur) => (cur !== "" && Number(cur) === value ? cur : value === undefined ? "" : String(value)));
  }, [value]);
  return (
    <div className="flex flex-wrap items-center gap-2">
      <Input
        type="number"
        inputMode="decimal"
        step={0.01}
        min={MIN_CONFIDENCE}
        max={MAX_CONFIDENCE}
        value={text}
        placeholder="例: 0.80"
        onChange={(e) => {
          const raw = e.target.value;
          setText(raw);
          if (raw.trim() === "") return onChange(undefined);
          const n = Number(raw);
          if (Number.isFinite(n)) onChange(Math.round(n * 100) / 100);
        }}
        data-testid="decide-threshold"
        {...a11y}
        className="h-8 w-24 font-mono tabular-nums max-md:text-base"
      />
      {value !== undefined ? (
        <span className="text-xs tabular-nums text-muted-foreground">{Math.round(value * 100)}%</span>
      ) : (
        <Button
          type="button"
          variant="outline"
          size="sm"
          className="h-8 text-xs pointer-coarse:h-11"
          onClick={() => onChange(STARTING_CONFIDENCE)}
          data-testid="decide-threshold-start"
        >
          0.80 から始める
        </Button>
      )}
    </div>
  );
}

/// 出口の鮮度: 今の設定のもの・検証の応答待ち・今の設定では導けない（前の出口を残している）。
type ExitsState = "fresh" | "pending" | "unresolved";

/// 出口の一覧（サーバが導いたもの）。応答が来るまで・設定が契約に合わない間は、その旨を出す。古い出口を今の出口と
/// 取り違えないよう、今の設定のものでなければ薄く描いて注記する。
function ExitsPreview({
  node,
  ports,
  state,
  hasRoute,
}: {
  node: IrNode;
  ports: readonly string[] | undefined;
  state: ExitsState;
  hasRoute: boolean;
}) {
  const stale = state !== "fresh";
  return (
    <div className="space-y-1.5" data-testid="decide-exits" data-state={state}>
      <p className="text-xs font-medium text-foreground">出口</p>
      {ports && ports.length > 0 ? (
        <>
          <ul className={cn("flex flex-wrap gap-1.5", stale && "opacity-60")} aria-describedby={stale ? "decide-exits-stale" : undefined}>
            {ports.map((port) => (
              <li
                key={port}
                className="rounded-full border bg-background px-2 py-0.5 text-[11px]"
                data-port={port}
              >
                {nodePortLabel(node, port)}
                {nodePortLabel(node, port) !== port ? (
                  <span className="ml-1 font-mono text-muted-foreground">{port}</span>
                ) : null}
              </li>
            ))}
          </ul>
          {stale ? (
            <p id="decide-exits-stale" className="text-[11px] text-muted-foreground" data-testid="decide-exits-stale">
              {state === "unresolved"
                ? "今の設定では出口が決まりません。上は前の設定の出口です（設定を直すと更新されます）"
                : "設定を変えました。確かめた後に出口を更新します"}
            </p>
          ) : null}
        </>
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
