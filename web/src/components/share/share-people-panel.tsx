"use client";

/// 共有ダイアログの「メンバー・部署」部分（ドライブとアーティファクトで共用）。
///
/// - 相手の種別（メンバー / 部署・ロール）を選んで検索し、**行ごとに権限（閲覧/編集。テーブルは追加だけも）を
///   選んで**共有する。選べる権限は呼び出し側が `roles` で渡す。
/// - 共有中の相手は表示名で出し（ID は出さない）、行の中で権限を変えたり解除したりできる。
///   表示名・email は共有一覧 API がテナント内ディレクトリから解決して返す（#741）。返らない相手
///   （ディレクトリ未登録など）は「名前を取得できないメンバー」＋短縮 ID で区別できるようにする。
/// - 検索は先頭ページのみ（全件取得しない）。別テナントの相手はサーバ側で出ない。
///
/// 付与・解除の API は呼び出し側から受け取る（ドライブ=ノード、アーティファクト=artifact）。
/// 共有中の相手の一覧が 403（共有を管理できるのは持ち主だけ）なら、空の一覧と付与の欄を出さず、そのことを知らせる
/// （黙って空に見せると、付与して初めて 403 になる・BUG-78）。403 以外で読み込めなかったときも、空の一覧
/// （まだ誰にも共有していません）に見せず、読み込めなかったことと、もう一度読み込むボタンを出す（BUG-120）。

import * as React from "react";
import { AlertCircle, Loader2, Lock, RotateCcw, Search, UserPlus, UserRound, Users, X } from "lucide-react";

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
import type { TableShareRole } from "@/lib/data-api";
import { searchDirectory, searchRoles, type ShareRole } from "@/lib/storage";
import { cn } from "@/lib/utils";

export type PrincipalKind = "user" | "role";
/// ドライブ・アーティファクトの共有の権限（既定・Rust の ShareRole から生成した型）。
export type BasicPeopleRole = ShareRole;
/// 共有の権限。submitter（追加だけ）はテーブルの共有だけが使う（D-113・Rust の TableShareRole から生成した型）。
export type PeopleRole = TableShareRole | BasicPeopleRole;
/// 選べる権限の 1 つ（`hint` は一覧の中だけに出す補足）。
export type PeopleRoleOption<R extends PeopleRole = PeopleRole> = { value: R; label: string; hint?: string };
export type PeopleTarget = { type: PrincipalKind; id: string };
/// 共有一覧の 1 件。`display_name` / `email` はサーバが解決できた時だけ入る（email は user のみ）。
export type PeopleEntry<R extends PeopleRole = PeopleRole> = {
  target: PeopleTarget;
  role: R;
  display_name?: string | null;
  email?: string | null;
};

/// ドライブ・アーティファクトの共有の権限（既定）。
const DEFAULT_ROLES: PeopleRoleOption<BasicPeopleRole>[] = [
  { value: "viewer", label: "閲覧" },
  { value: "editor", label: "編集" },
];

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

/// 共有を管理できない人（一覧が 403）に出す文（既定はテーブル・ドライブなどの持ち主）。
const DEFAULT_OWNER_ONLY = {
  title: "共有を管理できるのは持ち主だけです",
  body: "共有する相手を変えたいときは、持ち主に頼んでください。",
};

export function SharePeoplePanel<R extends PeopleRole = BasicPeopleRole>({
  open,
  resourceId,
  load,
  grant,
  revoke,
  roles = DEFAULT_ROLES as PeopleRoleOption<R>[],
  kinds = ["user", "role"],
  ownerOnly = DEFAULT_OWNER_ONLY,
}: {
  /// 開いた時に状態を戻して共有中の相手を読み直す。
  open: boolean;
  resourceId: string;
  load: () => Promise<PeopleEntry<R>[]>;
  grant: (target: PeopleTarget, role: R) => Promise<void>;
  revoke: (target: PeopleTarget, role: R) => Promise<void>;
  /// 選べる権限（先頭が既定）。省略時は閲覧・編集。
  roles?: PeopleRoleOption<R>[];
  /// 共有できる相手の種別（先頭が既定）。1 つだけなら種別の切り替えを出さない（アプリのテーブルは部署だけ・GAP-68）。
  kinds?: PrincipalKind[];
  /// 共有を管理できない人に出す文。
  ownerOnly?: { title: string; body: string };
}) {
  const defaultRole = (roles[0]?.value ?? "viewer") as R;
  const roleLabel = (r: R) => roles.find((o) => o.value === r)?.label ?? r;
  const firstKind = kinds[0] ?? "user";
  const [kind, setKind] = React.useState<PrincipalKind>(firstKind);
  const [query, setQuery] = React.useState("");
  const [results, setResults] = React.useState<Candidate[]>([]);
  const [searching, setSearching] = React.useState(false);
  // 行ごとに選んだ権限（未選択は閲覧）。
  const [rowRole, setRowRole] = React.useState<Record<string, R>>({});
  const [shares, setShares] = React.useState<PeopleEntry<R>[]>([]);
  const [loadingShares, setLoadingShares] = React.useState(false);
  // 共有を管理する権限が無い（一覧が 403）。
  const [forbidden, setForbidden] = React.useState(false);
  // 一覧を 403 以外で読み込めなかった（空の一覧に見せない）。
  const [loadFailed, setLoadFailed] = React.useState(false);
  // 「もう一度読み込む」で一覧だけを読み直す印。
  const [loadKey, setLoadKey] = React.useState(0);
  const [pendingKey, setPendingKey] = React.useState<string | null>(null);
  const loadRef = React.useRef(load);
  loadRef.current = load;

  // 開いたら状態リセット。
  React.useEffect(() => {
    if (!open) return;
    setKind(firstKind);
    setQuery("");
    setResults([]);
    setRowRole({});
  }, [open, resourceId, firstKind]);

  // 開いたら（と「もう一度読み込む」で）現在の共有相手を読む。
  React.useEffect(() => {
    if (!open) return;
    let active = true;
    setForbidden(false);
    setLoadFailed(false);
    setLoadingShares(true);
    loadRef
      .current()
      .then((s) => active && setShares(s))
      .catch((e: unknown) => {
        if (!active) return;
        setShares([]);
        const isForbidden = (e as { status?: number } | null)?.status === 403;
        setForbidden(isForbidden);
        setLoadFailed(!isForbidden);
      })
      .finally(() => active && setLoadingShares(false));
    return () => {
      active = false;
    };
  }, [open, resourceId, loadKey]);

  // 種別を切り替えたら検索状態をリセットする（user↔role で結果の意味が変わるため）。
  React.useEffect(() => {
    setQuery("");
    setResults([]);
  }, [kind]);

  // インクリメンタル検索（デバウンス。全件取得はせず先頭ページのみ）。active フラグで世代を守り、
  // 古いクエリ/種別の遅延レスポンスが新しい結果を上書きしないようにする。
  React.useEffect(() => {
    if (!open || forbidden) return;
    let active = true;
    const handle = setTimeout(() => {
      setSearching(true);
      const req =
        kind === "user"
          ? searchDirectory({ q: query, limit: 8 }).then((res) =>
              res.items.map((u) => ({ id: u.id, primary: u.display_name, secondary: u.email })),
            )
          : searchRoles({ q: query, limit: 8 }).then((res) =>
              res.items.map((r) => ({ id: r.id, primary: r.display_name, secondary: "部署・ロール" })),
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
  }, [open, query, kind, forbidden]);

  // (type, id, role) で既存共有を判定する（同じ役割のみ「付与済み」。役割の昇格は許可）。
  const sharedKeys = React.useMemo(() => new Set(shares.map(entryKey)), [shares]);

  // 読み直せたら、読み込めなかった表示も消す（付与の後に、いま付与した相手も含めた一覧を見せる）。
  const refresh = async () => {
    const next = await loadRef.current();
    setShares(next);
    setLoadFailed(false);
  };

  const share = async (candidate: Candidate) => {
    const role = rowRole[candidate.id] ?? defaultRole;
    const target = { type: kind, id: candidate.id };
    const k = entryKey({ target, role });
    setPendingKey(k);
    try {
      await grant(target, role);
      await refresh();
      toast({
        title: `「${candidate.primary}」と共有しました`,
        description: `権限: ${roleLabel(role)}`,
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

  const remove = async (entry: PeopleEntry<R>) => {
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
  const changeRole = async (entry: PeopleEntry<R>, role: R) => {
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

  if (forbidden) {
    return (
      <div
        role="status"
        data-testid="share-owner-only"
        className="flex min-w-0 items-start gap-2.5 rounded-lg border border-border bg-muted/40 px-3 py-3 text-sm"
      >
        <Lock className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden />
        <div className="min-w-0">
          <p className="font-medium">{ownerOnly.title}</p>
          <p className="mt-0.5 text-muted-foreground">{ownerOnly.body}</p>
        </div>
      </div>
    );
  }

  return (
    // min-w-0: grid（DialogContent）の子として、メール等の折り返さない文字幅まで広がらないように。
    <div className="flex min-w-0 flex-col gap-3">
      {kinds.length > 1 ? (
        <SegmentedControl
          aria-label="共有先の種別"
          size="sm"
          options={KIND_OPTIONS.filter((o) => kinds.includes(o.value as PrincipalKind))}
          value={kind}
          onValueChange={(v) => setKind(v as PrincipalKind)}
          className="self-start"
        />
      ) : null}

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
              const role = rowRole[c.id] ?? defaultRole;
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
                    roles={roles}
                    onChange={(r) => setRowRole((prev) => ({ ...prev, [c.id]: r }))}
                    label={`${c.primary} と共有する権限`}
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
        ) : loadFailed ? (
          <div
            role="alert"
            data-testid="share-load-failed"
            className="flex min-w-0 flex-col items-start gap-2 rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2.5 text-sm"
          >
            <p className="flex items-start gap-2 font-medium text-destructive">
              <AlertCircle className="mt-0.5 size-4 shrink-0" aria-hidden />
              共有している相手を読み込めませんでした
            </p>
            <p className="text-muted-foreground">少し待ってから、もう一度読み込んでください。</p>
            <Button type="button" size="sm" variant="outline" onClick={() => setLoadKey((k) => k + 1)}>
              <RotateCcw className="size-4" aria-hidden />
              もう一度読み込む
            </Button>
          </div>
        ) : shares.length === 0 ? (
          <p className="py-1 text-sm text-muted-foreground">まだ誰にも共有していません。</p>
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
                  <PrincipalAvatar kind={s.target.type} />
                  <div className="min-w-0 flex-1">
                    <p className="truncate text-sm font-medium">{name ?? fallback}</p>
                    <p className="truncate text-xs text-muted-foreground" title={shortId ? s.target.id : undefined}>
                      {secondary}
                    </p>
                  </div>
                  <RoleSelect
                    value={s.role}
                    roles={roles}
                    onChange={(r) => void changeRole(s, r)}
                    disabled={pendingKey === k}
                    label={`${a11yName} の権限`}
                  />
                  <button
                    type="button"
                    aria-label={`${a11yName} の共有を解除`}
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

/// 権限の小さな選択。行の中に収まる幅で、タッチでは高さ 36px（行全体で 44px 超）。欄には名前だけを出し、
/// 補足（`hint`）は開いた一覧の中にだけ出す。
function RoleSelect<R extends PeopleRole>({
  value,
  roles,
  onChange,
  disabled,
  label,
}: {
  value: R;
  roles: PeopleRoleOption<R>[];
  onChange: (role: R) => void;
  disabled?: boolean;
  label: string;
}) {
  // 4 文字の名前（追加だけ）があるときだけ欄を広げる（閲覧・編集だけの共有は今の幅のまま）。
  const wide = roles.some((o) => o.label.length > 2);
  return (
    <Select value={value} onValueChange={(v) => onChange(v as R)} disabled={disabled}>
      <SelectTrigger
        aria-label={label}
        className={cn(
          "h-8 shrink-0 gap-1 px-2 text-xs pointer-coarse:h-9 pointer-coarse:text-[13px]",
          wide ? "w-[6.25rem]" : "w-[4.75rem]",
        )}
      >
        <SelectValue>{roles.find((o) => o.value === value)?.label ?? value}</SelectValue>
      </SelectTrigger>
      <SelectContent align="end" className={cn(wide ? "w-80 max-w-[calc(100vw-2rem)]" : "min-w-[7rem]")}>
        {roles.map((o) => (
          <SelectItem key={o.value} value={o.value} data-testid={`share-role-${o.value}`}>
            {o.hint ? (
              <span className="flex flex-col gap-0.5 py-0.5">
                <span>{o.label}</span>
                <span className="text-xs text-muted-foreground">{o.hint}</span>
              </span>
            ) : (
              o.label
            )}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}
