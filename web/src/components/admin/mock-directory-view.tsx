"use client";

import * as React from "react";
import { Check, FileUp, Network, Search, Users, WandSparkles } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Badge } from "@/components/ui/badge";
import { Dialog, DialogContent, DialogDescription, DialogTitle } from "@/components/ui/dialog";
import { AdminPage, Panel } from "./admin-page";
import { DirectoryHierarchy } from "./directory-hierarchy";
import { DirectoryImportDialog } from "./directory-import-dialog";
import { AD_GROUPS, AD_USERS, INITIAL_MAPPING, groupMembers, inheritedRoles, sameMapping } from "./mock-directory-model";
import { IconInput } from "@/components/ui/icon-input";
import { SegmentedControl } from "@/components/ui/segmented-control";
import { DirectoryRoleBrowser } from "./directory-role-browser";
import { DirectoryAutoDialog } from "./directory-auto-dialog";
import { DEFAULT_ROLE_CATALOG } from "./directory-role-catalog";

/** ADの配置・入れ子とシキのロールの関係を別々に編集・確認するMock。 */
export function MockDirectoryView() {
  const [catalog, setCatalog] = React.useState(DEFAULT_ROLE_CATALOG);
  const [appliedCatalog, setAppliedCatalog] = React.useState(DEFAULT_ROLE_CATALOG);
  const roleChanges = React.useMemo(() => catalog.roles.filter((r) => { const old = appliedCatalog.index.get(r.id); return !old || r.label !== old.label || r.parent !== old.parent; }), [catalog, appliedCatalog]);
  const roleLabel = (id: string) => catalog.index.get(id)?.label ?? id;
  const [draft, setDraft] = React.useState(INITIAL_MAPPING);
  const [applied, setApplied] = React.useState(INITIAL_MAPPING);
  const [selected, setSelected] = React.useState(AD_GROUPS[1].id);
  const [mode, setMode] = React.useState<"ou" | "membership">("ou");
  const [query, setQuery] = React.useState("");
  const [unmappedOnly, setUnmappedOnly] = React.useState(false);
  const [autoOpen, setAutoOpen] = React.useState(false);
  const [importOpen, setImportOpen] = React.useState(false);
  const [connectionOpen, setConnectionOpen] = React.useState(false);
  const [message, setMessage] = React.useState("");
  const [user, setUser] = React.useState(AD_USERS[1]);
  const [nested, setNested] = React.useState(true);
  const [tested, setTested] = React.useState(false);
  const [connection, setConnection] = React.useState({ host: "ldaps://ad.example.co.jp:636", base: "DC=example,DC=co,DC=jp" });
  const dirty = !sameMapping(draft, applied) || roleChanges.length > 0;
  const changes = AD_GROUPS.filter((g) => !sameMapping({ ...applied, [g.id]: draft[g.id] }, applied));
  const mapped = AD_GROUPS.filter((g) => draft[g.id].length).length;
  const group = AD_GROUPS.find((g) => g.id === selected)!;
  const parents = AD_GROUPS.filter((g) => g.groups.includes(selected));
  const memberships = AD_GROUPS.filter((g) => groupMembers(g.id, nested).includes(user));
  const direct = [...new Set(memberships.flatMap((g) => draft[g.id]))];
  const inherited = inheritedRoles(direct, catalog.index);

  const roleDetail = React.useRef<HTMLDivElement>(null);
  const summary = (ids: string[]) => ids.slice(0, 3).map(catalog.rolePathLabel).join("、") + (ids.length > 3 ? ` ほか${ids.length - 3}件` : "");
  const chooseGroup = (id: string) => {
    setSelected(id);
    if (window.matchMedia("(max-width: 1023px)").matches) requestAnimationFrame(() => roleDetail.current?.scrollIntoView({ behavior: "smooth", block: "start" }));
  };
  return (
    <AdminPage title="AD連携" description="ディレクトリのグループと、シキのロールの対応を管理します。" wide actions={<>
      <Button variant="outline" size="sm" onClick={() => setConnectionOpen(true)}><Network aria-hidden />接続設定</Button>
      <Button variant="outline" size="sm" onClick={() => setAutoOpen(true)}><WandSparkles aria-hidden />ADから自動作成</Button>
      <Button variant="outline" size="sm" onClick={() => setImportOpen(true)}><FileUp aria-hidden />CSVインポート</Button>
    </>}>
      <div className="flex flex-wrap items-center gap-x-4 gap-y-2 text-xs text-muted-foreground">
        <Badge variant="muted">Mock</Badge><span>example.co.jp</span><span>{tested ? "接続テスト済み" : "サンプルデータ"}</span><span className="tabular-nums">対応済み {mapped} / {AD_GROUPS.length}グループ</span><span className="ml-auto tabular-nums">ロール {catalog.count.toLocaleString("ja-JP")}件</span>
      </div>
      <Panel>
        <div className="grid lg:grid-cols-[minmax(280px,0.85fr)_minmax(0,1.6fr)]">
          <section className="min-w-0 border-b lg:border-b-0 lg:border-r" aria-labelledby="directory-source-title">
            <div className="space-y-3 border-b px-4 py-3">
              <div className="flex items-center justify-between"><h2 id="directory-source-title" className="text-sm font-semibold">ADグループ</h2><span className="text-xs text-muted-foreground">{AD_GROUPS.length}件</span></div>
              <SegmentedControl size="sm" aria-label="ADグループの表示方法" value={mode} onValueChange={(value) => setMode(value as "ou" | "membership")} options={[{value:"ou",label:"OUで探す"},{value:"membership",label:"グループの入れ子"}]} />
              <IconInput icon={Search} type="search" aria-label="ADグループを検索" value={query} onValueChange={setQuery} placeholder="グループ名・OU名で検索" className="h-9" />
              <label className="flex items-center gap-2 text-xs text-muted-foreground"><input type="checkbox" checked={unmappedOnly} onChange={(e) => setUnmappedOnly(e.target.checked)} />未対応のみ（{AD_GROUPS.length - mapped}）</label>
            </div>
            <div className="max-h-[360px] overflow-y-auto overscroll-contain p-2 lg:h-[476px] lg:max-h-none"><DirectoryHierarchy mode={mode} query={query} unmappedOnly={unmappedOnly} selected={selected} mapping={draft} onSelect={chooseGroup} /></div>
            <p className="border-t px-4 py-2.5 text-[11px] leading-5 text-muted-foreground">{mode === "ou" ? "OUは配置です。配下にあるだけではロールは付与されません。" : "同じグループが複数の親に所属する場合があります。"}</p>
          </section>
          <section ref={roleDetail} className="min-w-0 scroll-mt-4" aria-labelledby="directory-target-title">
            <div className="border-b px-4 py-3">
              <div className="flex flex-wrap items-center justify-between gap-2"><h2 id="directory-target-title" className="flex items-center gap-2 text-sm font-semibold"><Users className="size-4 text-muted-foreground" aria-hidden />{group.name}</h2><Badge variant={draft[selected].length ? "muted" : "warning"}>{draft[selected].length ? `${draft[selected].length}ロールを選択` : "未対応"}</Badge></div>
              <p className="mt-1 text-[11px] leading-5 text-muted-foreground">{group.ou} · 直接所属 {group.members.length}人 / 入れ子を含む {groupMembers(group.id, true).length}人</p>
            </div>
            <DirectoryRoleBrowser catalog={catalog} groupName={group.name} onSaveRole={(value, assign) => { setCatalog(catalog.save(value)); if (assign) setDraft({ ...draft, [selected]: [...new Set([...draft[selected], value.id])] }); setMessage("ロールの変更を編集内容に追加しました。対応付けと一緒にMockに反映できます。"); }} selected={draft[selected]} groupId={selected} onChange={(roles) => setDraft({ ...draft, [selected]: roles })} />
            <p className="border-t px-4 py-2.5 text-[11px] leading-5 text-muted-foreground">チェックしたロールを対応付けます。子のメンバーは親にも含まれます。</p>
          </section>
        </div>
      </Panel>
      <details className="rounded-lg border bg-card">
        <summary className="cursor-pointer px-4 py-3 text-[13px] font-medium">ユーザーへの反映を確認</summary>
        <div className="grid gap-4 border-t p-4 md:grid-cols-[minmax(0,1fr)_minmax(0,1.4fr)]">
          <div><label htmlFor="ad-user" className="mb-2 block text-xs font-medium">確認するユーザー</label><select id="ad-user" value={user} onChange={(e) => setUser(e.target.value)} className="h-9 w-full rounded-md border bg-background px-3 text-[13px]">{AD_USERS.map((name) => <option key={name}>{name}</option>)}</select><label className="mt-3 flex items-center gap-2 text-xs"><input type="checkbox" checked={nested} onChange={(e) => setNested(e.target.checked)} />ADグループの入れ子を展開して試す</label><p className="mt-2 text-[11px] leading-5 text-muted-foreground">実際の所属取得はKeycloakの設定に依存します。</p></div>
          <div className="space-y-3 text-xs"><div><p className="text-muted-foreground">{nested ? "入れ子を含む" : "直接所属する"}ADグループ</p><p className="mt-1">{memberships.map((g) => g.name).join("、") || "なし"}</p></div><div><p className="text-muted-foreground">対応付けによるロール</p><p className="mt-1 font-medium" data-testid="directory-direct-roles">{direct.map(roleLabel).slice(0, 10).join("、") || "対応するロールがありません。"}{direct.length > 10 && ` ほか${direct.length - 10}件`}</p></div><div><p className="text-muted-foreground">シキの親子関係から追加されるロール</p><p className="mt-1" data-testid="directory-inherited-roles">{inherited.map(roleLabel).slice(0, 10).join("、") || "なし"}</p></div></div>
        </div>
      </details>
      <details className="text-xs text-muted-foreground"><summary className="cursor-pointer py-1">所属関係・連携仕様</summary><div className="mt-2 space-y-2 rounded-lg border p-4 leading-6"><p>{group.name} / objectGUID: <code className="break-all">{group.id}</code></p><p>所属する親グループ: {parents.map((g) => g.name).join("、") || "なし"}</p><p>シキのclaim同期は直接所属を扱います。グループのパスからロールの親子関係を自動生成する実装はありません。表示の階層はサンプルの明示的な関係です。</p><p>Keycloak 26.2でADの階層を保持すると、複数親や循環を含むグループで同期が失敗し得ます。OUの配置とグループ所属は別々に扱います。</p></div></details>
      {dirty && <div className="sticky bottom-2 z-10 rounded-lg border bg-card shadow-sm">
        <div className="flex flex-wrap items-center justify-between gap-3 px-4 py-3"><p className="text-[13px] font-medium">{changes.length}グループ・{roleChanges.length}ロールに未反映の変更</p><div className="flex gap-2"><Button size="sm" variant="outline" onClick={() => { setDraft(applied); setCatalog(appliedCatalog); setMessage("未反映の変更を取り消しました。"); }}>変更を取り消す</Button><Button size="sm" onClick={() => { setApplied(draft); setAppliedCatalog(catalog); setMessage(`${changes.length}グループの対応付けをMockに反映しました。${roleChanges.length ? ` ロール${roleChanges.length}件の変更も反映しました。` : ""}`); }}><Check aria-hidden />Mockに反映</Button></div></div>
        <details className="border-t px-4 py-2 text-xs"><summary className="cursor-pointer text-muted-foreground">変更内容を確認</summary><ul className="mt-2 max-h-40 divide-y overflow-auto">{changes.map((g) => <li key={g.id} className="py-2"><p className="font-medium">{g.name}</p><p className="mt-1 text-muted-foreground">{summary(applied[g.id]) || "対応なし"} → {summary(draft[g.id]) || "対応なし"}</p></li>)}{roleChanges.map((role) => <li key={role.id} className="py-2"><p className="font-medium">{appliedCatalog.index.has(role.id) ? "ロール変更" : "ロール追加"}: {role.label}</p><p className="mt-1 text-muted-foreground">{appliedCatalog.index.has(role.id) ? appliedCatalog.rolePathLabel(role.id) : "新規"} → {catalog.rolePathLabel(role.id)}</p></li>)}</ul></details>
      </div>}
      <p role="status" aria-live="polite" className="text-xs text-muted-foreground">{message || "変更はこの画面内だけに反映されます。実際のAD・権限への反映や永続保存は行いません。"}</p>
      {autoOpen && <DirectoryAutoDialog catalog={catalog} mapping={draft} selected={selected} onClose={() => setAutoOpen(false)} onAccept={(next, mapping) => { setCatalog(next); setDraft(mapping); setAutoOpen(false); setMessage("ADからの自動作成を編集内容に取り込みました。Mockに反映する前に確認・取消できます。"); }} />}
      {importOpen && <DirectoryImportDialog catalog={catalog} current={draft} onClose={() => setImportOpen(false)} onImport={(mapping) => { setDraft(mapping); setImportOpen(false); setMessage("CSVを編集内容に取り込みました。変更内容を確認してからMockに反映してください。"); }} />}
      {connectionOpen && <ConnectionDialog connection={connection} onConnectionChange={setConnection} tested={tested} onTested={setTested} onClose={() => setConnectionOpen(false)} />}
    </AdminPage>
  );
}

function ConnectionDialog({ connection, onConnectionChange, tested, onTested, onClose }: {
  connection: { host: string; base: string };
  onConnectionChange: (value: { host: string; base: string }) => void;
  tested: boolean; onTested: (tested: boolean) => void; onClose: () => void;
}) {
  const { host, base } = connection;
  const [failure, setFailure] = React.useState(false);
  const [busy, setBusy] = React.useState(false);
  const [message, setMessage] = React.useState(tested ? "接続確認済み（Mock）" : "");
  const timer = React.useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  React.useEffect(() => () => clearTimeout(timer.current), []);
  return <Dialog open onOpenChange={(open) => { if (!open) onClose(); }}><DialogContent>
    <DialogTitle>AD接続設定</DialogTitle><DialogDescription>Keycloak経由のLDAP連携を想定したMockです。実際の接続・設定保存は行いません。</DialogDescription>
    <label className="space-y-2 text-sm"><span>接続先（LDAPS）</span><Input disabled={busy} value={host} onChange={(e) => { onConnectionChange({ ...connection, host: e.target.value }); onTested(false); setMessage(""); }} /></label>
    <label className="space-y-2 text-sm"><span>検索ベースDN</span><Input disabled={busy} value={base} onChange={(e) => { onConnectionChange({ ...connection, base: e.target.value }); onTested(false); setMessage(""); }} /></label>
    <label className="flex items-center gap-2 text-xs"><input type="checkbox" disabled={busy} checked={failure} onChange={(e) => { setFailure(e.target.checked); onTested(false); setMessage(""); }} />接続失敗をシミュレート</label>
    <p className="text-xs text-muted-foreground">画面上のグループはサンプルです。対応表は接続テストなしでも編集できます。</p>
    <p role="status" aria-live="polite" className="text-sm">{message}</p>
    <div className="flex justify-end gap-2"><Button variant="outline" onClick={onClose}>閉じる</Button><Button disabled={busy || !/^ldaps:\/\/[^\s/]+$/.test(host) || !base.trim()} onClick={() => {
      setBusy(true); onTested(false); setMessage("");
      timer.current = setTimeout(() => { setBusy(false); onTested(!failure); setMessage(failure ? "接続テスト失敗（Mock）。接続先と証明書を確認してください。" : "接続テスト成功（Mock）。"); }, 700);
    }}>{busy ? "接続を確認中…" : "接続をテスト"}</Button></div>
  </DialogContent></Dialog>;
}
