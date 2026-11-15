"use client";
import * as React from "react";
import { ArrowRight, ChevronLeft } from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogTitle,
  DialogDescription,
} from "@/components/ui/dialog";
import { SegmentedControl } from "@/components/ui/segmented-control";
import { Source } from "./source";
import { MappingDiff, RoleTree, ReviewSources } from "./review";
import { post, hasChanges, type Plan, type Group } from "./api";

export function AutoDialog({
  selected,
  revision,
  onClose,
  onAccept,
}: {
  selected?: Group;
  revision: number;
  onClose: () => void;
  onAccept: (plan: Plan) => void;
}) {
  const [scope, setScope] = React.useState("selected");
  const [ids, setIds] = React.useState<string[]>(selected ? [selected.id] : []);
  const [mode, setMode] = React.useState("ou");
  const [review, setReview] = React.useState(false);
  const [plan, setPlan] = React.useState<Plan>();
  const [active, setActive] = React.useState<Group | undefined>(selected);
  const [busy, setBusy] = React.useState(false);
  const [error, setError] = React.useState("");
  React.useEffect(() => {
    let cancelled = false;
    setPlan(undefined);
    setError("");
    if (scope === "selected" && !ids.length) {
      setBusy(false);
      return;
    }
    setBusy(true);
    const timer = setTimeout(() => {
      post<Plan>("/auto-plan", {
        group_ids: scope === "all" ? null : ids,
        hierarchy: mode,
      })
        .then((next) => {
          if (!cancelled) {
            setPlan(next);
            setActive(
              (old) =>
                next.sources.find((g) => g.id === old?.id) ?? next.sources[0],
            );
          }
        })
        .catch((e) => {
          if (!cancelled) setError(e.message);
        })
        .finally(() => {
          if (!cancelled) setBusy(false);
        });
    }, 350);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [scope, mode, ids]);
  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open) onClose();
      }}
    >
      <DialogContent className="flex h-[min(800px,calc(100dvh-64px))] max-w-6xl flex-col gap-0 overflow-hidden p-0 max-sm:h-[94dvh] max-sm:max-h-[94dvh] max-sm:px-0 max-sm:pb-0 max-sm:pt-0">
        <header className="shrink-0 border-b px-5 py-4 pr-12">
          <div className="mb-2 flex items-center gap-2 text-[11px] text-muted-foreground">
            <span className={!review ? "font-medium text-foreground" : ""}>
              1 対象を選ぶ
            </span>
            <ArrowRight className="size-3" aria-hidden />
            <span className={review ? "font-medium text-foreground" : ""}>
              2 内容を確認
            </span>
          </div>
          <DialogTitle className="text-base">ADからロールを作成</DialogTitle>
          <DialogDescription className="mt-1 text-xs">
            {review
              ? "元の名前・階層と対応付けを確認してください。まだ反映されていません。"
              : "選択した分、またはすべてのグループを、ADの名前と階層でロールにします。"}
          </DialogDescription>
        </header>
        <div className="grid min-h-0 flex-1 grid-cols-1 overflow-y-auto md:grid-cols-[300px_minmax(0,1fr)] md:overflow-hidden">
          <aside className="flex min-h-0 flex-col border-b bg-muted/15 md:border-b-0 md:border-r">
            {review && plan ? (
              <ReviewSources
                sources={plan.sources}
                active={active?.id}
                onSelect={setActive}
              />
            ) : (
              <>
                <div className="space-y-3 border-b p-4">
                  <SegmentedControl
                    size="sm"
                    aria-label="自動作成の対象"
                    value={scope}
                    onValueChange={setScope}
                    options={[
                      {
                        value: "selected",
                        label: `選択した分 ${ids.length}件`,
                      },
                      { value: "all", label: "すべて" },
                    ]}
                  />
                  <fieldset className="space-y-2">
                    <legend className="mb-2 text-[11px] font-medium">
                      反映する階層
                    </legend>
                    {[
                      { value: "ou", label: "OUの配置とグループ名" },
                      { value: "group", label: "グループの入れ子と名前" },
                    ].map((item) => (
                      <label
                        key={item.value}
                        className="flex items-center gap-2 text-xs"
                      >
                        <input
                          type="radio"
                          name="ad-hierarchy"
                          value={item.value}
                          checked={mode === item.value}
                          onChange={() => setMode(item.value)}
                        />
                        {item.label}
                      </label>
                    ))}
                  </fieldset>
                  <p className="text-[10px] leading-5 text-muted-foreground">
                    既存の対応は保持して追加します。グループの入れ子は接続設定で保持した階層を使用します。
                  </p>
                </div>
                <div className="flex min-h-52 flex-1 flex-col">
                  <Source
                    revision={revision}
                    selected={active?.id}
                    onSelect={setActive}
                    checked={ids}
                    disabled={scope === "all"}
                    onCheck={(group, checked) => {
                      setActive(group);
                      setIds((old) =>
                        checked
                          ? [...old, group.id]
                          : old.filter((id) => id !== group.id),
                      );
                    }}
                  />
                </div>
              </>
            )}
          </aside>
          <section
            className="flex min-h-80 min-w-0 flex-col md:min-h-0"
            aria-busy={busy}
          >
            {busy ? (
              <p role="status" className="p-5 text-xs text-muted-foreground">
                作成案を検証中…
              </p>
            ) : error ? (
              <p
                role="alert"
                className="p-5 text-xs leading-6 text-destructive"
              >
                {error}
              </p>
            ) : plan ? (
              <>
                <RoleTree
                  key={plan.changes.idempotency_key}
                  nodes={plan.nodes}
                  changes={plan.changes}
                />
                <MappingDiff
                  group={active}
                  changes={plan.changes}
                  nodes={plan.nodes}
                />
              </>
            ) : (
              <p className="p-5 text-xs text-muted-foreground">
                左側で作成元のADグループを選択してください。
              </p>
            )}
          </section>
        </div>
        <footer className="flex shrink-0 flex-wrap items-center justify-between gap-3 border-t px-5 py-3">
          <p className="text-xs text-muted-foreground">
            {plan
              ? `${plan.group_count}グループ / 新規ロール ${plan.changes.roles.length}件 / 既存 ${plan.existing_roles}件`
              : "対象を選択"}
          </p>
          <div className="flex gap-2">
            {review ? (
              <>
                <Button variant="outline" onClick={() => setReview(false)}>
                  <ChevronLeft aria-hidden />
                  対象を変更
                </Button>
                <Button
                  disabled={!plan || !hasChanges(plan.changes)}
                  onClick={() => plan && onAccept(plan)}
                >
                  編集内容に取り込む
                </Button>
              </>
            ) : (
              <>
                <Button variant="outline" onClick={onClose}>
                  キャンセル
                </Button>
                <Button
                  disabled={busy || !plan || !hasChanges(plan.changes)}
                  onClick={() => setReview(true)}
                >
                  {plan && !hasChanges(plan.changes)
                    ? plan.group_count
                      ? "すべて作成・対応済み"
                      : "同期済みグループがありません"
                    : "内容を確認"}
                </Button>
              </>
            )}
          </div>
        </footer>
      </DialogContent>
    </Dialog>
  );
}
