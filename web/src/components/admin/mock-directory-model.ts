/** Mockのサンプル。実際のAD/Keycloakの構成を取得したものではない。
 * OUの配置とグループ所属は別々に保持する。roleの辺はOpenFGAの
 * parent#member@child#memberに相当し、名前やパスからは推論しない。
 * 根拠: crates/authz/model/authorization-model.fga, routes/auth/callback.rs,
 * https://learn.microsoft.com/en-us/windows-server/identity/ad-ds/manage/understand-security-groups
 * https://learn.microsoft.com/en-us/windows-server/identity/ad-ds/plan/reviewing-ou-design-concepts
 */
export const MOCK_ROLES = [
  { id: "all", label: "全社", parent: null, members: 48 },
  { id: "planning", label: "経営企画", parent: "all", members: 3 },
  { id: "sales", label: "営業部", parent: "all", members: 24 },
  { id: "sales-1", label: "営業1課", parent: "sales", members: 11 },
  { id: "sales-2", label: "営業2課", parent: "sales", members: 9 },
  { id: "dev", label: "開発部", parent: "all", members: 14 },
];
export type Mapping = Record<string, string[]>;
export const AD_GROUPS = [
  { id: "00000000-0000-4000-8000-000000000001", name: "SG-Sales", ou: "営業本部", members: ["小林 大輔"], groups: ["00000000-0000-4000-8000-000000000002", "00000000-0000-4000-8000-000000000003"] },
  { id: "00000000-0000-4000-8000-000000000002", name: "SG-Sales-1", ou: "営業本部/営業1課", members: ["佐藤 美咲"], groups: [] },
  { id: "00000000-0000-4000-8000-000000000003", name: "SG-Sales-2", ou: "営業本部/営業2課", members: ["高橋 直樹"], groups: [] },
  { id: "00000000-0000-4000-8000-000000000004", name: "SG-Engineering", ou: "開発本部", members: ["鈴木 健太"], groups: [] },
  { id: "00000000-0000-4000-8000-000000000005", name: "SG-Planning", ou: "コーポレート", members: ["山田 葵"], groups: [] },
  // 同じグループが複数の親に属する例。OUの配置とは独立。
  { id: "00000000-0000-4000-8000-000000000006", name: "SG-Project-A", ou: "プロジェクト", members: [], groups: ["00000000-0000-4000-8000-000000000002", "00000000-0000-4000-8000-000000000004"] },
];
export const INITIAL_MAPPING: Mapping = Object.fromEntries(AD_GROUPS.map((g, i) => [g.id, [["sales"], ["sales-1"], ["sales-2"], ["dev"], [], []][i]]));
export const AD_USERS = [...new Set(AD_GROUPS.flatMap((g) => g.members))];
export function sameMapping(a: Mapping, b: Mapping) {
  return AD_GROUPS.every((g) => [...a[g.id]].sort().join(",") === [...b[g.id]].sort().join(","));
}
export function groupMembers(id: string, nested: boolean, visited = new Set<string>()): string[] {
  if (visited.has(id)) return [];
  const next = new Set(visited).add(id);
  const g = AD_GROUPS.find((group) => group.id === id);
  return g ? [...new Set([...g.members, ...(nested ? g.groups.flatMap((child) => groupMembers(child, true, next)) : [])])] : [];
}
export function inheritedRoles(direct: string[], index = DIRECTORY_ROLE_INDEX): string[] {
  const result = new Set(direct);
  const pending = [...direct];
  for (let i = 0; i < pending.length; i++) {
    const parent = index.get(pending[i])?.parent;
    if (parent && !result.has(parent)) { result.add(parent); pending.push(parent); }
  }
  return [...result].filter((id) => !direct.includes(id));
}
export const roleLabel = (id: string) => DIRECTORY_ROLE_INDEX.get(id)?.label ?? id;

// 大規模操作確認用。既存ロール画面の小さなfixtureと分けておく。
export const DIRECTORY_ROLES = [
  ...MOCK_ROLES,
  ...Array.from({ length: 100 }, (_, division) => {
    const number = String(division + 1).padStart(3, "0");
    const id = `division-${number}`;
    return [
      { id, label: `事業部${number}`, parent: "all", members: 990 },
      ...Array.from({ length: 99 }, (_, team) => ({
        id: `${id}/team-${String(team + 1).padStart(3, "0")}`,
        label: `チーム${String(team + 1).padStart(3, "0")}`,
        parent: id, members: 10,
      })),
    ];
  }).flat(),
];
export const DIRECTORY_ROLE_INDEX = new Map(DIRECTORY_ROLES.map((role) => [role.id, role]));
