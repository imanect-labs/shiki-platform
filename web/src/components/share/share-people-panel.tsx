"use client";

/// 共有ダイアログの「メンバー・部署」部分（ドライブとアーティファクトで共用）。
///
/// - 相手の種別（メンバー / 部署・ロール）を選んで検索し、**行ごとに権限（閲覧/編集）を選んで**共有する。
/// - 共有中の相手は表示名で出し（ID は出さない）、行の中で権限を変えたり解除したりできる。
/// - 検索は先頭ページのみ（全件取得しない）。別テナントの相手はサーバ側で出ない。
///
/// 付与・解除の API は呼び出し側から受け取る（ドライブ=ノード、アーティファクト=artifact）。

import * as React from "react";
import { Loader2, Search, UserPlus, UserRound, Users, X } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { SegmentedControl } from "@/components/ui/segmented-control";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { toast } from "@/components/ui/use-toast";
import { searchDirectory, searchRoles } from "@/lib/storage";
import { cn } from "@/lib/utils";

import {
  principalName,
  rememberPrincipal,
  useResolvePrincipals,
  type PrincipalKind,
} from "./principal-names";

export type PeopleRole = "viewer" | "editor";
export type PeopleTarget = { type: PrincipalKind; id: string };
export type PeopleEntry = { target: PeopleTarget; role: PeopleRole };

const ROLE_LABEL: Record<PeopleRole, string> = { viewer: "閲覧", editor: "編集" };

const KINDS: { value: PrincipalKind; label: string; placeholder: string; empty: string }[] = [
  {
    value: "user",
    label: "メンバー",
    placeholder: "名前・メールで検索",
    empty: "該当するメンバーがいません",
  },
  {
    value: "role",
    label: "部署・ロール",
    placeholder: "部署・ロール名で検索",
    empty: "該当する部署・ロールがありません",
  },
];
const KIND_OPTIONS = [
  { value: "user", label: "メンバー", icon: UserPlus, testId: "share-kind-user" },
  { value: "role", label: "部署・ロール", icon: Users, testId: "share-kind-role" },
];

/// 検索結果を種別非依存に正規化した 1 候補。
type Candidate = { id: string; primary: string; secondary: string };

const entryKey = (e: { target: PeopleTarget; role: PeopleRole }) =>
  `${e.target.type}:${e.target.id}:${e.role}`;

export function SharePeoplePanel({
  open,
  resourceId,
  load,
  grant,
  revoke,
}: {
  /// 開いた時に状態を戻して共有中の相手を読み直す。
  open: boolean;
  resourceId: string;
  load: () => Promise<PeopleEntry[]>;
  grant: (target: PeopleTarget, role: PeopleRole) => Promise<void>;
  revoke: (target: PeopleTarget, role: PeopleRole) => Promise<void>;
}) {
  const [kind, setKind] = React.useState<PrincipalKind>("user");
  const [query, setQuery] = React.useState("");
  const [results, setResults] = React.useState<Candidate[]>([]);
  const [searching, setSearching] = React.useState(false);
  // 行ごとに選んだ権限（未選択は閲覧）。
  const [rowRole, setRowRole] = React.useState<Record<string, PeopleRole>>({});
  const [shares, setShares] = React.useState<PeopleEntry[]>([]);
  const [loadingShares, setLoadingShares] = React.useState(false);
  const [pendingKey, setPendingKey] = React.useState<string | null>(null);
  const loadRef = React.useRef(load);
  loadRef.current = load;

  // 開いたら状態リセット＋現在の共有相手を読む。
  React.useEffect(() => {
    if (!open) return;
    let active = true;
    setKind("user");
    setQuery("");
    setResults([]);
    setRowRole({});
    setLoadingShares(true);
    loadRef
      .current()
      .then((s) => active && setShares(s))
      .catch(() => active && setShares([]))
      .finally(() => active && setLoadingShares(false));
    return () => {
      active = false;
    };
  }, [open, resourceId]);

  // 種別を切り替えたら検索状態をリセットする（user↔role で結果の意味が変わるため）。
  React.useEffect(() => {
    setQuery("");
    setResults([]);
  }, [kind]);

  // インクリメンタル検索（デバウンス。全件取得はせず先頭ページのみ）。active フラグで世代を守り、
  // 古いクエリ/種別の遅延レスポンスが新しい結果を上書きしないようにする。
  React.useEffect(() => {
    if (!open) return;
    let active = true;
    const handle = setTimeout(() => {
      setSearching(true);
      const req =
        kind === "user"
          ? searchDirectory({ q: query, limit: 8 }).then((res) =>
              res.items.map((u) => {
                rememberPrincipal("user", u.id, { name: u.display_name, secondary: u.email });
                return { id: u.id, primary: u.display_name, secondary: u.email };
              }),
            )
          : searchRoles({ q: query, limit: 8 }).then((res) =>
              res.items.map((r) => {
                rememberPrincipal("role", r.id, { name: r.display_name, secondary: "部署・ロール" });
                return { id: r.id, primary: r.display_name, secondary: "部署・ロール" };
              }),
            );
      req
        .then((items) => active && setResults(items))
        .catch(() => active && setResults([]))
        .finally(() => active && setSearching(false));
    }, 200);
    return () => {
      active = false;
      clearTimeout(handle);
    };
  }, [open, query, kind]);

  // (type, id, role) で既存共有を判定する（同じ役割のみ「付与済み」。役割の昇格は許可）。
  const sharedKeys = React.useMemo(() => new Set(shares.map(entryKey)), [shares]);
  useResolvePrincipals(shares.map((s) => s.target));

  const refresh = async () => setShares(await loadRef.current());

  const share = async (candidate: Candidate) => {
    const role = rowRole[candidate.id] ?? "viewer";
    const target = { type: kind, id: candidate.id };
    const k = entryKey({ target, role });
    setPendingKey(k);
    try {
      await grant(target, role);
      await refresh();
      toast({
        title: "共有しました",
        description: `${candidate.primary} に${ROLE_LABEL[role]}権限を付与`,
      });
    } catch (e) {
      toast({
        variant: "destructive",
        title: "共有に失敗しました",
        description: e instanceof Error ? e.message : String(e),
      });
    } finally {
      setPendingKey(null);
    }
  };

  const remove = async (entry: PeopleEntry) => {
    const k = entryKey(entry);
    setPendingKey(k);
    try {
      await revoke(entry.target, entry.role);
      // 解除した (type, id, role) の行だけを消す。同相手の別ロール共有はサーバ側に残るため、
      // id だけで filter すると画面から消えて「完全に外せた」と誤認する（実際は残存）。
      setShares((prev) => prev.filter((s) => entryKey(s) !== k));
    } catch (e) {
      toast({
        variant: "destructive",
        title: "解除に失敗しました",
        description: e instanceof Error ? e.message : String(e),
      });
    } finally {
      setPendingKey(null);
    }
  };

  /// 共有中の相手の権限を変える（新しい権限を付けてから古い権限を外す・途中で失敗しても
  /// アクセスが消えない順序）。
  const changeRole = async (entry: PeopleEntry, role: PeopleRole) => {
    if (role === entry.role) return;
    const k = entryKey(entry);
    setPendingKey(k);
    try {
      if (!sharedKeys.has(entryKey({ target: entry.target, role }))) await grant(entry.target, role);
      await revoke(entry.target, entry.role);
      await refresh();
    } catch (e) {
      toast({
        variant: "destructive",
        title: "権限を変更できませんでした",
        description: e instanceof Error ? e.message : String(e),
      });
      await refresh().catch(() => undefined);
    } finally {
      setPendingKey(null);
    }
  };

  const kindDef = KINDS.find((k) => k.value === kind)!;

  return (
    // min-w-0: grid（DialogContent）の子として、メール等の折り返さない文字幅まで広がらないように。
    <div className="flex min-w-0 flex-col gap-3">
      <SegmentedControl
        aria-label="共有先の種別"
        size="sm"
        options={KIND_OPTIONS}
        value={kind}
        onValueChange={(v) => setKind(v as PrincipalKind)}
        className="self-start"
      />

      {/* 検索 */}
      <div className="relative">
        <Search
          className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground"
          aria-hidden
        />
        <Input
          type="search"
          enterKeyHint="search"
          autoCapitalize="none"
          autoCorrect="off"
          spellCheck={false}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => {
            // 検索は打鍵に追従するので、確定はキーボードを閉じるだけ（フォーム送信させない）。
            if (e.key === "Enter") {
              e.preventDefault();
              (e.target as HTMLInputElement).blur();
            }
          }}
          placeholder={kindDef.placeholder}
          aria-label={kindDef.placeholder}
          className="pl-9 max-md:h-11"
        />
      </div>

      {/* 検索結果（最大 8 件なので内側でスクロールさせない） */}
      <div className="rounded-lg border border-border" aria-busy={searching || undefined}>
        {searching && results.length === 0 ? (
          <div className="flex items-center justify-center gap-2 py-5 text-sm text-muted-foreground">
            <Loader2 className="size-4 animate-spin" aria-hidden />
            検索中…
          </div>
        ) : results.length === 0 ? (
          <p className="px-3 py-5 text-center text-sm text-muted-foreground">{kindDef.empty}</p>
        ) : (
          <ul className={cn("divide-y divide-border", searching && "opacity-60")}>
            {results.map((c) => {
              const role = rowRole[c.id] ?? "viewer";
              const k = `${kind}:${c.id}:${role}`;
              const already = sharedKeys.has(k);
              return (
                <li key={c.id} className="flex items-center gap-2.5 px-3 py-2">
                  <PrincipalAvatar kind={kind} />
                  <div className="min-w-0 flex-1">
                    <p className="truncate text-sm font-medium">{c.primary}</p>
                    <p className="truncate text-xs text-muted-foreground">{c.secondary}</p>
                  </div>
                  <RoleSelect
                    value={role}
                    onChange={(r) => setRowRole((prev) => ({ ...prev, [c.id]: r }))}
                    label={`${c.primary} に付与する権限`}
                  />
                  <Button
                    type="button"
                    size="sm"
                    variant={already ? "ghost" : "outline"}
                    disabled={already || pendingKey === k}
                    onClick={() => void share(c)}
                    className="shrink-0 pointer-coarse:h-9"
                  >
                    {pendingKey === k ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
                    {already ? "共有済み" : "共有"}
                  </Button>
                </li>
              );
            })}
          </ul>
        )}
      </div>

      {/* 共有中の相手 */}
      <div className="flex flex-col gap-1.5">
        <p className="text-sm font-medium">共有中の相手</p>
        {loadingShares ? (
          <div className="flex items-center gap-2 py-2 text-sm text-muted-foreground">
            <Loader2 className="size-4 animate-spin" aria-hidden />
            読み込み中…
          </div>
        ) : shares.length === 0 ? (
          <p className="py-1 text-sm text-muted-foreground">まだ誰にも共有していません。</p>
        ) : (
          <ul className="flex flex-col divide-y divide-border rounded-lg border border-border">
            {shares.map((s) => {
              const k = entryKey(s);
              const resolved = principalName(s.target.type, s.target.id);
              const fallback = s.target.type === "role" ? s.target.id : "名前を取得できないメンバー";
              return (
                <li key={k} data-testid="share-entry" className="flex items-center gap-2.5 py-1.5 pl-3 pr-1">
                  <PrincipalAvatar kind={s.target.type} />
                  <div className="min-w-0 flex-1">
                    <p className="truncate text-sm font-medium">{resolved?.name ?? fallback}</p>
                    <p className="truncate text-xs text-muted-foreground">
                      {resolved?.secondary ?? (s.target.type === "role" ? "部署・ロール" : "メンバー")}
                    </p>
                  </div>
                  <RoleSelect
                    value={s.role}
                    onChange={(r) => void changeRole(s, r)}
                    disabled={pendingKey === k}
                    label={`${resolved?.name ?? fallback} の権限`}
                  />
                  <button
                    type="button"
                    aria-label={`${resolved?.name ?? fallback} の共有を解除`}
                    disabled={pendingKey === k}
                    onClick={() => void remove(s)}
                    className="flex size-9 shrink-0 items-center justify-center rounded-md text-muted-foreground outline-none transition-colors hover:bg-destructive/10 hover:text-destructive focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-50 pointer-coarse:size-11"
                  >
                    {pendingKey === k ? (
                      <Loader2 className="size-4 animate-spin" aria-hidden />
                    ) : (
                      <X className="size-4" aria-hidden />
                    )}
                  </button>
                </li>
              );
            })}
          </ul>
        )}
      </div>
    </div>
  );
}

function PrincipalAvatar({ kind }: { kind: PrincipalKind }) {
  const Icon = kind === "role" ? Users : UserRound;
  return (
    <span className="flex size-8 shrink-0 items-center justify-center rounded-full bg-secondary text-secondary-foreground">
      <Icon className="size-4" aria-hidden />
    </span>
  );
}

/// 権限（閲覧/編集）の小さな選択。行の中に収まる幅で、タッチでは高さ 36px（行全体で 44px 超）。
function RoleSelect({
  value,
  onChange,
  disabled,
  label,
}: {
  value: PeopleRole;
  onChange: (role: PeopleRole) => void;
  disabled?: boolean;
  label: string;
}) {
  return (
    <Select value={value} onValueChange={(v) => onChange(v as PeopleRole)} disabled={disabled}>
      <SelectTrigger
        aria-label={label}
        className="h-8 w-[4.75rem] shrink-0 gap-1 px-2 text-xs pointer-coarse:h-9 pointer-coarse:text-[13px]"
      >
        <SelectValue />
      </SelectTrigger>
      <SelectContent align="end" className="min-w-[7rem]">
        <SelectItem value="viewer">閲覧</SelectItem>
        <SelectItem value="editor">編集</SelectItem>
      </SelectContent>
    </Select>
  );
}
