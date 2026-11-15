/** サーバの検索・子一覧取得を模擬する境界。実APIの契約ではない。
 * 全件はMockカタログ内でのみ保持し、UIには1ページ最大40件を返す。
 * 現行APIはdirectory_roleの検索/keyset pagingのみ。管理API接続時には
 * tenant/orgの絞込み、親子タプル取得、サーバ側検索とcursorが必要。
 */
import { DIRECTORY_ROLES } from "./mock-directory-model";

export type CatalogRole = typeof DIRECTORY_ROLES[number];
export const ROLE_PAGE_SIZE = 40;
export const ROLE_COUNT = DIRECTORY_ROLES.length;
export type RoleQuery = { query: string; parent: string | null; selectedOnly: boolean; selected: string[]; page: number; all?: boolean; excludeBranch?: string };
export type RolePage = { rows: CatalogRole[]; total: number; page: number; pages: number };
export interface RoleCatalog {
  roles: CatalogRole[]; index: Map<string, CatalogRole>; count: number;
  rolePath: (id: string) => CatalogRole[]; rolePathLabel: (id: string) => string;
  childCount: (id: string) => number; query: (request: RoleQuery) => RolePage;
  fetchPage: (request: RoleQuery, signal: AbortSignal) => Promise<RolePage>;
  save: (value: CatalogRole) => RoleCatalog;
}
export function createRoleCatalog(roles: CatalogRole[] = DIRECTORY_ROLES): RoleCatalog {
const index = new Map(roles.map((role) => [role.id, role]));
const children = new Map<string | null, CatalogRole[]>();
for (const role of roles) {
  const list = children.get(role.parent) ?? [];
  list.push(role); children.set(role.parent, list);
}
function rolePath(id: string): CatalogRole[] {
  const path: CatalogRole[] = [];
  const seen = new Set<string>();
  let role = index.get(id);
  while (role && !seen.has(role.id)) {
    seen.add(role.id); path.unshift(role);
    role = role.parent ? index.get(role.parent) : undefined;
  }
  return path;
}
const paths = new Map(roles.map((r) => [r.id, rolePath(r.id).map((part) => part.label).join(" / ")]));
const searchText = new Map(roles.map((r) => [r.id, `${r.id} ${paths.get(r.id)}`.toLowerCase()]));
const rolePathLabel = (id: string) => paths.get(id) ?? id;
const childCount = (id: string) => children.get(id)?.length ?? 0;
function queryRoleCatalog(request: RoleQuery): RolePage {
  const tokens = request.query.toLowerCase().trim().split(/\s+/).filter(Boolean);
  const source = request.selectedOnly
    ? request.selected.flatMap((id) => { const role = index.get(id); return role ? [role] : []; })
    : tokens.length || request.all ? roles : children.get(request.parent) ?? [];
  const matches = source.filter((r) => (!request.excludeBranch || !rolePath(r.id).some((part) => part.id === request.excludeBranch)) && tokens.every((token) => searchText.get(r.id)!.includes(token)));
  const pages = Math.max(1, Math.ceil(matches.length / ROLE_PAGE_SIZE));
  const page = Math.max(0, Math.min(request.page, pages - 1));
  return { rows: matches.slice(page * ROLE_PAGE_SIZE, (page + 1) * ROLE_PAGE_SIZE), total: matches.length, page, pages };
}
async function fetchRolePage(request: RoleQuery, signal: AbortSignal): Promise<RolePage> {
  await new Promise<void>((resolve, reject) => {
    if (signal.aborted) { reject(new DOMException("Aborted", "AbortError")); return; }
    const abort = () => { clearTimeout(timer); reject(new DOMException("Aborted", "AbortError")); };
    const timer = setTimeout(() => { signal.removeEventListener("abort", abort); resolve(); }, 180);
    signal.addEventListener("abort", abort, { once: true });
  });
  return queryRoleCatalog(request);
}

return { roles, index, count: roles.length, rolePath, rolePathLabel, childCount, query: queryRoleCatalog, fetchPage: fetchRolePage,
  save(value: CatalogRole) {
    if (!value.label.trim() || value.label.trim().length > 80) throw new Error("ロール名は1〜80文字で入力してください。");
    if (value.parent && !index.has(value.parent)) throw new Error("親ロールが見つかりません。");
    if (value.parent && rolePath(value.parent).some((part) => part.id === value.id)) throw new Error("自身や配下のロールを親にはできません。");
    if (roles.some((role) => role.id !== value.id && role.parent === value.parent && role.label.toLowerCase() === value.label.trim().toLowerCase())) throw new Error("同じ親に同名のロールがあります。");
    const next = { ...value, label: value.label.trim() };
    return createRoleCatalog(index.has(value.id) ? roles.map((role) => role.id === value.id ? next : role) : [...roles, next]);
  },
};
}
export const DEFAULT_ROLE_CATALOG = createRoleCatalog();
export const rolePathLabel = DEFAULT_ROLE_CATALOG.rolePathLabel;
