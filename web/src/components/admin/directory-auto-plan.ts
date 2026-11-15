import { AD_GROUPS, type Mapping } from "./mock-directory-model";
import { createRoleCatalog, type CatalogRole, type RoleCatalog } from "./directory-role-catalog";

export type AutoMode = "ou" | "membership";
export type PlanNode = { role: CatalogRole; path: string; source: "OU" | "グループ"; action: "create" | "reuse" };
export type PlanRow = { group: typeof AD_GROUPS[number]; before: string[]; after: string[]; target: string; path: string };
export type AutoPlan = { nodes: PlanNode[]; rows: PlanRow[]; errors: string[]; included: number; changed: number; catalog: RoleCatalog; mapping: Mapping };

/** OU配置を明示的なロール関係に変換する場合も、ADの所属とは区別する。
 * グループの入れ子は単一親Mockで表せる部分だけ。複数親・循環を勝手に平坦化しない。
 * 入力は変更せず、確認用の計画を返す。安定IDとパスの照合で再実行を冪等にする。
 */
export function planAutoRoles(catalog: RoleCatalog, mapping: Mapping, selected: string[], mode: AutoMode, descendants: boolean, groups = AD_GROUPS): AutoPlan {
  const ids = new Set(selected);
  const errors: string[] = [];
  const byId = new Map(groups.map((g) => [g.id, g]));
  const parents = new Map(groups.map((g) => [g.id, groups.filter((p) => p.groups.includes(g.id)).map((p) => p.id)]));
  if (mode === "membership") {
    // 配下の取込は利用者が選んだ枝だけ。後で補う親の別の枝には広げない。
    if (descendants) {
      const queue = [...ids];
      for (let i = 0; i < queue.length; i++) for (const child of byId.get(queue[i])?.groups ?? []) {
        if (!ids.has(child)) { ids.add(child); queue.push(child); }
      }
    }
    const ancestors = [...ids];
    for (let i = 0; i < ancestors.length; i++) for (const parent of parents.get(ancestors[i]) ?? []) {
      if (!ids.has(parent)) { ids.add(parent); ancestors.push(parent); }
    }
    for (const id of ids) if ((parents.get(id)?.length ?? 0) > 1) errors.push(`${byId.get(id)?.name}: 親が複数あります（${parents.get(id)!.map((p) => byId.get(p)?.name).join("、")}）。単一親のロールMockではそのまま再現できません。`);
  }
  const nodes: PlanNode[] = [], rows: PlanRow[] = [];
  const index = new Map(catalog.index);
  const siblings = new Map(catalog.roles.map((r) => [`${r.parent ?? ""}\0${r.label}`, r]));
  const owners = new Map<string, string>();
  const resolved = new Map<string, CatalogRole>();
  const paths = new Map<string, string>();
  const root = catalog.index.has("all") ? "all" : null;
  const rootPath = root ? catalog.rolePathLabel(root) : "最上位";
  const add = (stable: string, label: string, parent: string | null, source: PlanNode["source"]): CatalogRole => {
    if (resolved.has(stable)) return resolved.get(stable)!;
    const stableExisting = index.get(stable);
    if (stableExisting && (stableExisting.label !== label || stableExisting.parent !== parent)) errors.push(`${label}: 前回作成したロールの名前または親が変更されています。自動で上書きできません。`);
    const found = stableExisting ?? siblings.get(`${parent ?? ""}\0${label}`);
    const role = found ?? { id: stable, label, parent, members: 0 };
    if (owners.has(role.id) && owners.get(role.id) !== stable) errors.push(`${label}: 異なるAD項目が同じロールのパスになります。対象を確認してください。`);
    owners.set(role.id, stable);
    const path = `${parent ? paths.get(parent) ?? catalog.rolePathLabel(parent) : "最上位"} / ${label}`;
    if (!label || label.length > 80) errors.push(`${label}: ロール名は1〜80文字が必要です。`);
    index.set(role.id, role); siblings.set(`${parent ?? ""}\0${label}`, role);
    resolved.set(stable, role); paths.set(role.id, path);
    if (!nodes.some((node) => node.role.id === role.id)) nodes.push({ role, path, source, action: found ? "reuse" : "create" });
    return role;
  };
  const visiting = new Set<string>();
  const groupsDone = new Map<string, CatalogRole>();
  const groupRole = (id: string): CatalogRole | null => {
    if (groupsDone.has(id)) return groupsDone.get(id)!;
    const group = byId.get(id);
    if (!group) { errors.push(`ADグループ ${id} が見つかりません。`); return null; }
    if (visiting.has(id)) { errors.push(`${group.name}: グループの所属関係が循環しています。`); return null; }
    visiting.add(id);
    let parent = root;
    if (mode === "ou") {
      let path = "";
      for (const part of group.ou.split("/")) {
        path += `/${part}`;
        parent = add(`ad-ou-${encodeURIComponent(path)}`, part, parent, "OU").id;
      }
    } else {
      const parentIds = parents.get(id) ?? [];
      if (parentIds.length === 1 && ids.has(parentIds[0])) {
        const parentRole = groupRole(parentIds[0]);
        if (!parentRole) { visiting.delete(id); return null; }
        parent = parentRole.id;
      }
      if (parentIds.length > 1) { visiting.delete(id); return null; }
    }
    const role = add(`ad-group-${group.id}`, group.name, parent, "グループ");
    groupsDone.set(id, role); visiting.delete(id); return role;
  };
  const nextMapping = { ...mapping };
  for (const group of groups.filter((g) => ids.has(g.id))) {
    const role = groupRole(group.id);
    if (!role) continue;
    const before = mapping[group.id];
    const after = [...new Set([...before, role.id])];
    nextMapping[group.id] = after;
    rows.push({ group, before, after, target: role.id, path: paths.get(role.id) ?? rootPath });
  }
  const uniqueErrors = [...new Set(errors)];
  const changed = rows.filter((r) => r.after.length !== r.before.length).length;
  return { nodes, rows, errors: uniqueErrors, included: ids.size, changed,
    catalog: uniqueErrors.length ? catalog : createRoleCatalog([...catalog.roles, ...nodes.filter((n) => n.action === "create").map((n) => n.role)]),
    mapping: uniqueErrors.length ? mapping : nextMapping };
}
