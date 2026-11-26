"use client";

/// 共有ダイアログの「メンバー・部署」部分（ドライブ・データ表・アーティファクトで共用）。
///
/// - 検索欄は 1 つで、メンバーと部署・ロールをまとめて探す（以前は種別の切り替えが要った）。
///   付ける権限（閲覧/編集）は検索欄の横で 1 回だけ選び、候補の「共有」で付ける。
/// - 候補は検索語を入れたときだけ出す。空のときは「共有中の相手」を検索欄のすぐ下に見せる。
/// - 既に共有中の相手は候補側にも「閲覧で共有中」と出し、選んだ権限が違えばその場で変更できる
///   （同じ相手に閲覧と編集の 2 つの共有が並ばないように）。
/// - 共有中の相手は頭文字のアバター・表示名・メールで出し（ID は出さない）、行の中で権限を変えたり
///   解除したりできる。表示名・email は共有一覧 API がテナント内ディレクトリから解決して返す（#741）。
///   返らない相手は「名前を取得できないメンバー」＋短縮 ID で区別できるようにする。
/// - 検索は先頭ページのみ（全件取得しない）。別テナントの相手はサーバ側で出ない。
///
/// 付与・解除の API は呼び出し側から受け取る（ドライブ=ノード、アーティファクト=artifact）。

import * as React from "react";
import { Loader2, Search, Users, X } from "lucide-react";

import { Avatar, AvatarFallback } from "@/components/ui/avatar";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
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

export type PrincipalKind = "user" | "role";
export type PeopleRole = "viewer" | "editor";
export type PeopleTarget = { type: PrincipalKind; id: string };
/// 共有一覧の 1 件。`display_name` / `email` はサーバが解決できた時だけ入る（email は user のみ）。
export type PeopleEntry = {
  target: PeopleTarget;
  role: PeopleRole;
  display_name?: string | null;
  email?: string | null;
};

const ROLE_LABEL: Record<PeopleRole, string> = { viewer: "閲覧", editor: "編集" };
const SEARCH_PLACEHOLDER = "名前・メール・部署で検索";
/// 1 回の検索で出す候補数（メンバー / 部署・ロール）。先頭ページのみで全件は取らない。
const USER_LIMIT = 6;
const ROLE_LIMIT = 4;

/// 検索結果を種別込みで正規化した 1 候補。
type Candidate = { kind: PrincipalKind; id: string; primary: string; secondary: string };

const entryKey = (e: { target: PeopleTarget; role: PeopleRole }) =>
  `${e.target.type}:${e.target.id}:${e.role}`;
const targetKey = (t: PeopleTarget) => `${t.type}:${t.id}`;

export function SharePeoplePanel({
  open,
  resourceId,
  load,
  grant,
  revoke,
  onCountChange,
}: {
  /// 開いた時に状態を戻して共有中の相手を読み直す。
  open: boolean;
  resourceId: string;
  load: () => Promise<PeopleEntry[]>;
  grant: (target: PeopleTarget, role: PeopleRole) => Promise<void>;
  revoke: (target: PeopleTarget, role: PeopleRole) => Promise<void>;
  /// 共有中の相手の件数（読み込み中は null）。タブの件数表示に使う。
  onCountChange?: (count: number | null) => void;
}) {
  const [query, setQuery] = React.useState("");
  const [results, setResults] = React.useState<Candidate[]>([]);
  const [searching, setSearching] = React.useState(false);
  // これから付ける権限（検索欄の横で 1 回だけ選ぶ）。
  const [addRole, setAddRole] = React.useState<PeopleRole>("viewer");
  const [shares, setShares] = React.useState<PeopleEntry[]>([]);
  const [loadingShares, setLoadingShares] = React.useState(false);
  const [pendingKey, setPendingKey] = React.useState<string | null>(null);
  const loadRef = React.useRef(load);
  loadRef.current = load;
  const countRef = React.useRef(onCountChange);
  countRef.current = onCountChange;

  // 開いたら状態リセット＋現在の共有相手を読む。
  React.useEffect(() => {
    if (!open) return;
    let active = true;
    setQuery("");
    setResults([]);
    setAddRole("viewer");
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

  React.useEffect(() => {
    countRef.current?.(loadingShares ? null : shares.length);
  }, [loadingShares, shares.length]);

  // インクリメンタル検索（デバウンス。メンバーと部署・ロールを並行に先頭ページだけ引く）。
  // 検索語が空のときは候補を出さない。active フラグで世代を守り、古いクエリの遅延レスポンスが
  // 新しい結果を上書きしないようにする。片方の検索が失敗しても、もう片方の結果は出す。
  React.useEffect(() => {
    if (!open) return;
    const q = query.trim();
    if (!q) {
      setResults([]);
      setSearching(false);
      return;
    }
    let active = true;
    const handle = setTimeout(() => {
      setSearching(true);
      Promise.allSettled([
        searchDirectory({ q, limit: USER_LIMIT }),
        searchRoles({ q, limit: ROLE_LIMIT }),
      ])
        .then(([users, roles]) => {
          if (!active) return;
          const items: Candidate[] = [];
          if (users.status === "fulfilled") {
            for (const u of users.value.items) {
              items.push({ kind: "user", id: u.id, primary: u.display_name, secondary: u.email });
            }
          }
          if (roles.status === "fulfilled") {
            for (const r of roles.value.items) {
              items.push({ kind: "role", id: r.id, primary: r.display_name, secondary: "部署・ロール" });
            }
          }
          setResults(items);
        })
        .finally(() => active && setSearching(false));
    }, 200);
    return () => {
      active = false;
      clearTimeout(handle);
    };
  }, [open, query]);

  // 相手ごとの現在の共有（同じ相手に閲覧と編集の両方があれば編集を代表にする）。
  const sharedByTarget = React.useMemo(() => {
    const m = new Map<string, PeopleEntry>();
    for (const s of shares) {
      const k = targetKey(s.target);
      const prev = m.get(k);
      if (!prev || (prev.role === "viewer" && s.role === "editor")) m.set(k, s);
    }
    return m;
  }, [shares]);
  const sharedKeys = React.useMemo(() => new Set(shares.map(entryKey)), [shares]);

  const refresh = async () => setShares(await loadRef.current());

  const share = async (candidate: Candidate) => {
    const role = addRole;
    const target = { type: candidate.kind, id: candidate.id };
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

  const hasQuery = query.trim().length > 0;
  const userResults = results.filter((c) => c.kind === "user");
  const roleResults = results.filter((c) => c.kind === "role");

  const renderCandidate = (c: Candidate) => {
    const existing = sharedByTarget.get(targetKey({ type: c.kind, id: c.id }));
    const k = existing ? entryKey(existing) : entryKey({ target: { type: c.kind, id: c.id }, role: addRole });
    const busy = pendingKey === k;
    return (
      <li key={`${c.kind}:${c.id}`} className="flex items-center gap-2.5 px-3 py-2">
        <PrincipalAvatar kind={c.kind} name={c.primary} />
        <div className="min-w-0 flex-1">
          <p className="truncate text-sm font-medium">{c.primary}</p>
          <p className="truncate text-xs text-muted-foreground">
            {existing ? (
              <span className="font-medium text-foreground/80">{ROLE_LABEL[existing.role]}で共有中</span>
            ) : (
              c.secondary
            )}
          </p>
        </div>
        {existing && existing.role === addRole ? (
          <Button type="button" size="sm" variant="ghost" disabled className="shrink-0 pointer-coarse:h-9">
            共有済み
          </Button>
        ) : existing ? (
          <Button
            type="button"
            size="sm"
            variant="outline"
            disabled={busy}
            onClick={() => void changeRole(existing, addRole)}
            className="shrink-0 pointer-coarse:h-9"
          >
            {busy ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
            {ROLE_LABEL[addRole]}に変更
          </Button>
        ) : (
          <Button
            type="button"
            size="sm"
            variant="outline"
            disabled={busy}
            onClick={() => void share(c)}
            className="shrink-0 pointer-coarse:h-9"
          >
            {busy ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
            共有
          </Button>
        )}
      </li>
    );
  };

  return (
    // min-w-0: grid（DialogContent）の子として、メール等の折り返さない文字幅まで広がらないように。
    <div className="flex min-w-0 flex-col gap-4">
      {/* 追加: 検索欄＋これから付ける権限 */}
      <div className="flex flex-col gap-2">
        <div className="flex items-center gap-2">
          <div className="relative min-w-0 flex-1">
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
              placeholder={SEARCH_PLACEHOLDER}
              aria-label="共有する相手を検索"
              data-share-search=""
              className="pl-9 max-md:h-11"
            />
          </div>
          <RoleSelect
            value={addRole}
            onChange={setAddRole}
            label="追加する相手の権限"
            size="lg"
          />
        </div>

        {/* 候補（検索語があるときだけ。メンバーと部署・ロールを分けて出す） */}
        {hasQuery ? (
          <div className="rounded-lg border border-border" aria-busy={searching || undefined}>
            {searching && results.length === 0 ? (
              <div className="flex items-center justify-center gap-2 py-5 text-sm text-muted-foreground">
                <Loader2 className="size-4 animate-spin" aria-hidden />
                検索中…
              </div>
            ) : results.length === 0 ? (
              <p className="px-3 py-5 text-center text-sm text-muted-foreground">
                該当するメンバー・部署がありません
              </p>
            ) : (
              <div className={cn("flex flex-col", searching && "opacity-60")}>
                {userResults.length > 0 ? (
                  <CandidateGroup label="メンバー">{userResults.map(renderCandidate)}</CandidateGroup>
                ) : null}
                {roleResults.length > 0 ? (
                  <CandidateGroup label="部署・ロール" divided={userResults.length > 0}>
                    {roleResults.map(renderCandidate)}
                  </CandidateGroup>
                ) : null}
              </div>
            )}
          </div>
        ) : null}
      </div>

      {/* 共有中の相手 */}
      <div className="flex flex-col gap-1.5">
        <p className="text-sm font-medium">
          共有中の相手
          {!loadingShares && shares.length > 0 ? (
            <span className="ml-1.5 text-xs font-normal text-muted-foreground">{shares.length} 件</span>
          ) : null}
        </p>
        {loadingShares ? (
          <div className="flex items-center gap-2 py-2 text-sm text-muted-foreground">
            <Loader2 className="size-4 animate-spin" aria-hidden />
            読み込み中…
          </div>
        ) : shares.length === 0 ? (
          <div className="flex items-center gap-3 rounded-lg border border-dashed border-border px-3 py-4 text-sm text-muted-foreground">
            <Users className="size-5 shrink-0" aria-hidden />
            <p>
              まだ誰にも共有していません。
              <span className="mt-0.5 block text-xs">上の検索欄から、メンバーや部署を探して共有できます。</span>
            </p>
          </div>
        ) : (
          <ul className="flex flex-col divide-y divide-border rounded-lg border border-border">
            {shares.map((s) => {
              const k = entryKey(s);
              const name = s.display_name ?? undefined;
              const fallback = s.target.type === "role" ? s.target.id : "名前を取得できないメンバー";
              // 名前を引けないメンバーが複数いても行（と操作ボタンの読み上げ）を区別できるよう、短縮 ID を添える。
              const shortId = !name && s.target.type !== "role" ? s.target.id.slice(0, 8) : null;
              const a11yName = shortId ? `${fallback}（ID ${shortId}）` : (name ?? fallback);
              const secondary =
                s.target.type === "role"
                  ? "部署・ロール"
                  : (s.email ?? (shortId ? `メンバー · ID ${shortId}…` : "メンバー"));
              return (
                <li key={k} data-testid="share-entry" className="flex items-center gap-2.5 py-1.5 pl-3 pr-1">
                  <PrincipalAvatar kind={s.target.type} name={name} />
                  <div className="min-w-0 flex-1">
                    <p className="truncate text-sm font-medium">{name ?? fallback}</p>
                    <p className="truncate text-xs text-muted-foreground" title={shortId ? s.target.id : undefined}>
                      {secondary}
                    </p>
                  </div>
                  <RoleSelect
                    value={s.role}
                    onChange={(r) => void changeRole(s, r)}
                    disabled={pendingKey === k}
                    label={`${a11yName} の権限`}
                  />
                  <button
                    type="button"
                    aria-label={`${a11yName} の共有を解除`}
                    title="共有を解除"
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

function CandidateGroup({
  label,
  divided,
  children,
}: {
  label: string;
  divided?: boolean;
  children: React.ReactNode;
}) {
  return (
    <div className={cn(divided && "border-t border-border")}>
      <p className="px-3 pb-0.5 pt-2 text-xs font-medium text-muted-foreground">{label}</p>
      <ul className="divide-y divide-border" aria-label={label}>
        {children}
      </ul>
    </div>
  );
}

/// 相手のアバター。メンバーは表示名の頭文字、部署・ロールは人の集まりのアイコン。
function PrincipalAvatar({ kind, name }: { kind: PrincipalKind; name?: string }) {
  const initial = name?.trim().charAt(0);
  return (
    <Avatar aria-hidden>
      <AvatarFallback
        className={cn(kind === "role" && "bg-secondary text-secondary-foreground")}
      >
        {kind === "role" || !initial ? <Users className="size-4" /> : initial.toUpperCase()}
      </AvatarFallback>
    </Avatar>
  );
}

/// 権限（閲覧/編集）の小さな選択。行の中に収まる幅で、タッチでは高さ 36px（行全体で 44px 超）。
/// `size="lg"` は検索欄の横に置く版（検索欄と同じ高さ）。
function RoleSelect({
  value,
  onChange,
  disabled,
  label,
  size = "sm",
}: {
  value: PeopleRole;
  onChange: (role: PeopleRole) => void;
  disabled?: boolean;
  label: string;
  size?: "sm" | "lg";
}) {
  return (
    <Select value={value} onValueChange={(v) => onChange(v as PeopleRole)} disabled={disabled}>
      <SelectTrigger
        aria-label={label}
        className={cn(
          "shrink-0 gap-1",
          size === "lg"
            ? "h-9 w-[5.5rem] px-3 text-sm max-md:h-11 pointer-coarse:h-11"
            : "h-8 w-[4.75rem] px-2 text-xs pointer-coarse:h-9 pointer-coarse:text-[13px]",
        )}
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
