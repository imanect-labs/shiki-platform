import {
  AUDIT_ACTIONS,
  AUDIT_CATEGORIES,
  type AuditAction,
  type AuditCategory,
} from "@/generated/audit-actions";

/// 監査ログの操作の語彙（#830）。表示名・分類は Rust の単一定義からの生成物（手書きしない）。
export { AUDIT_ACTIONS, AUDIT_CATEGORIES };
export type { AuditAction, AuditCategory };

const actionMap = new Map<string, (typeof AUDIT_ACTIONS)[number]>(AUDIT_ACTIONS.map((a) => [a.action, a]));
const categoryMap = new Map<string, string>(AUDIT_CATEGORIES.map((c) => [c.id, c.label]));

/// 操作の表示名（語彙に無い古い行は null・そのときは記録された文字列を出す）。
export function actionLabel(action: string): string | null {
  return actionMap.get(action)?.label ?? null;
}

/// 操作の分類の表示名（語彙に無い操作は null）。
export function actionCategoryLabel(action: string): string | null {
  const a = actionMap.get(action);
  return a ? (categoryMap.get(a.category) ?? null) : null;
}

export function isAuditCategory(v: string | null | undefined): v is AuditCategory {
  return v != null && categoryMap.has(v);
}

/// 絞り込みの候補（分類を選んでいればその分類の操作だけ）。
export function actionOptions(category?: AuditCategory | null): readonly (typeof AUDIT_ACTIONS)[number][] {
  return category ? AUDIT_ACTIONS.filter((a) => a.category === category) : AUDIT_ACTIONS;
}
