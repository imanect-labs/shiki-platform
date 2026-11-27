"use client";

/// 共有ダイアログの「メンバー・部署」部分（ドライブ・データ表・アーティファクトで共用）。
///
/// - 検索欄は 1 つで、メンバーと部署・ロールをまとめて探す（以前は種別の切り替えが要った）。
///   付ける権限（閲覧/編集）は検索欄の横で 1 回だけ選び、候補の「共有」で付ける。
/// - 検索欄の下は 1 つの領域で、検索語があれば候補、無ければ「共有中の相手」を出す（入れ替え）。
///   高さはダイアログ側で固定し、この領域だけがスクロールする（共有相手が多くても枠は伸びない）。
/// - 既に共有中の相手は候補側にも「閲覧で共有中」と出し、選んだ権限が違えばその場で変更できる
///   （同じ相手に閲覧と編集の 2 つの共有が並ばないように）。
/// - 共有中の相手は頭文字のアバター・表示名・メールで出し（ID は出さない）、行の中で権限を変えたり
///   解除したりできる。表示名・email は共有一覧 API がテナント内ディレクトリから解決して返す（#741）。
///   返らない相手は「名前を取得できないメンバー」＋短縮 ID で区別できるようにする。
/// - 検索は先頭ページのみ（全件取得しない）。別テナントの相手はサーバ側で出ない。
///
/// 付与・解除の API は呼び出し側から受け取る（ドライブ=ノード、アーティファクト=artifact）。

import * as React from "react";
import { Loader2, type LucideIcon, Search, Users, X } from "lucide-react";

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
import { seasonAccentStyle } from "@/lib/season";
import { cn } from "@/lib/utils";

import { GROUP_SEASON, ROLE_LABEL, ROLE_SEASON, RoleDot, RoleTag } from "./role-color";

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

const SEARCH_PLACEHOLDER = "名前や部署で検索";
/// 1 回の検索で出す候補数（メンバー / 部署・ロール）。先頭ページのみで全件は取らない。
const USER_LIMIT = 8;
const ROLE_LIMIT = 8;

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
  // 候補は「どの検索語の結果か」と一緒に持つ。いまの検索語の結果でなければ出さない（検索語を変えた直後に、
  // 前の語の候補を押して入力と違う相手に共有してしまわないように）。
  const [resultsFor, setResultsFor] = React.useState<{ q: string; items: Candidate[] }>({ q: "", items: [] });
  const [searching, setSearching] = React.useState(false);
  // これから付ける権限（検索欄の横で 1 回だけ選ぶ）。
  const [addRole, setAddRole] = React.useState<PeopleRole>("viewer");
  const [shares, setShares] = React.useState<PeopleEntry[]>([]);
  const [loadingShares, setLoadingShares] = React.useState(false);
  const [pendingKey, setPendingKey] = React.useState<string | null>(null);
  const loadRef = React.useRef(load);
  loadRef.current = load;
  // いま開いている共有先。処理の途中で共有先が切り替わったら、前の共有先の結果で一覧を上書きしない。
  const resourceRef = React.useRef(resourceId);
  resourceRef.current = resourceId;
  const countRef = React.useRef(onCountChange);
  countRef.current = onCountChange;

  // 開いたら状態リセット＋現在の共有相手を読む。
  React.useEffect(() => {
    if (!open) return;
    let active = true;
    setQuery("");
    setResultsFor({ q: "", items: [] });
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
      setResultsFor({ q: "", items: [] });
      setSearching(false);
      return;
    }
    let active = true;
    // 検索語が変わった時点で「検索中」にする（前の語の候補は下で隠れる）。
    setSearching(true);
    const handle = setTimeout(() => {
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
          setResultsFor({ q, items });
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

  const refresh = async () => {
    const forResource = resourceRef.current;
    const next = await loadRef.current();
    if (resourceRef.current === forResource) setShares(next);
  };

  // 付与・変更・解除は同時に 1 つだけ（state の更新を待たずに弾けるよう ref で持つ）。処理中に権限を
  // 切り替えて同じ相手をもう一度押し、閲覧と編集を重ねて付けてしまうのを防ぐ。
  const opRef = React.useRef(false);
  const runExclusive = async (key: string, fn: () => Promise<void>) => {
    if (opRef.current) return;
    opRef.current = true;
    setPendingKey(key);
    try {
      await fn();
    } finally {
      opRef.current = false;
      setPendingKey(null);
    }
  };

  const share = async (candidate: Candidate) => {
    const role = addRole;
    const target = { type: candidate.kind, id: candidate.id };
    await runExclusive(entryKey({ target, role }), async () => {
      try {
        await grant(target, role);
        await refresh();
        // 検索を閉じて共有中の相手の一覧へ戻す（いま足した相手がそこに見える）。
        setQuery("");
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
      }
    });
  };

  const remove = async (entry: PeopleEntry) => {
    const k = entryKey(entry);
    await runExclusive(k, async () => {
      try {
        const forResource = resourceRef.current;
        await revoke(entry.target, entry.role);
        if (resourceRef.current !== forResource) return;
        // 解除した (type, id, role) の行だけを消す。同相手の別ロール共有はサーバ側に残るため、
        // id だけで filter すると画面から消えて「完全に外せた」と誤認する（実際は残存）。
        setShares((prev) => prev.filter((s) => entryKey(s) !== k));
      } catch (e) {
        toast({
          variant: "destructive",
          title: "解除に失敗しました",
          description: e instanceof Error ? e.message : String(e),
        });
      }
    });
  };

  /// 共有中の相手の権限を変える。途中で失敗したときは権限が小さい側に倒れる順序にする
  /// （引き上げは付けてから外す、引き下げは外してから付ける）。
  const changeRole = async (
    entry: PeopleEntry,
    role: PeopleRole,
    opts?: { clearQuery?: boolean },
  ) => {
    if (role === entry.role) return;
    await runExclusive(entryKey(entry), async () => {
      try {
        const hasNew = sharedKeys.has(entryKey({ target: entry.target, role }));
        if (role === "editor") {
          // 引き上げ（閲覧→編集）: 先に編集を付けてから閲覧を外す（途中で失敗してもアクセスが消えない）。
          if (!hasNew) await grant(entry.target, role);
          await revoke(entry.target, entry.role);
        } else {
          // 引き下げ（編集→閲覧）: 先に編集を外してから閲覧を付ける。途中で失敗しても編集権限が残らない
          // （権限は小さい側に倒れる。閲覧の付与だけ失敗したら、一覧に出ないので付け直せる）。
          await revoke(entry.target, entry.role);
          if (!hasNew) await grant(entry.target, role);
        }
        await refresh();
        if (opts?.clearQuery) setQuery("");
      } catch (e) {
        toast({
          variant: "destructive",
          title: "権限を変更できませんでした",
          description: e instanceof Error ? e.message : String(e),
        });
        await refresh().catch(() => undefined);
      }
    });
  };

  const hasQuery = query.trim().length > 0;
  // いまの検索語の結果だけを出す（語を変えた直後の前の語の結果は出さない）。
  const results = resultsFor.q === query.trim() ? resultsFor.items : [];
  const userResults = results.filter((c) => c.kind === "user");
  const roleResults = results.filter((c) => c.kind === "role");
  // 処理中は全体の操作を止める。共有中の一覧を読み込み中は、既存の共有が分からないので候補から付けない。
  const anyPending = pendingKey !== null;
  const candidateLocked = anyPending || loadingShares;

  const renderCandidate = (c: Candidate) => {
    const existing = sharedByTarget.get(targetKey({ type: c.kind, id: c.id }));
    // 処理中の印は相手ごとに出す（権限を切り替えても同じ相手のボタンが押せるように戻らない）。
    const busy = pendingKey !== null && pendingKey.startsWith(`${targetKey({ type: c.kind, id: c.id })}:`);
    return (
      <li key={`${c.kind}:${c.id}`} className={ROW}>
        <PrincipalAvatar kind={c.kind} name={c.primary} />
        <div className="min-w-0 flex-1">
          <p className="truncate text-[13.5px] font-medium">{c.primary}</p>
          {/* 同名の相手を見分けられるよう、共有中でもメール等は消さずに状態を横に添える。 */}
          <p className="flex min-w-0 items-center gap-2 text-xs text-muted-foreground">
            <span className="min-w-0 truncate">{c.secondary}</span>
            {existing ? (
              <span className="shrink-0">
                <RoleTag role={existing.role}>{ROLE_LABEL[existing.role]}で共有中</RoleTag>
              </span>
            ) : null}
          </p>
        </div>
        {existing && existing.role === addRole ? (
          <span className="shrink-0 px-2 text-xs text-muted-foreground">共有済み</span>
        ) : existing ? (
          <Button
            type="button"
            size="sm"
            variant="outline"
            disabled={candidateLocked}
            onClick={() => void changeRole(existing, addRole, { clearQuery: true })}
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
            disabled={candidateLocked}
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
    // 高さはダイアログ側で固定し、ここは残りの高さを埋めて中だけスクロールする（検索・読み込み・
    // 件数の増減でダイアログの大きさが変わらない）。min-w-0: メール等の折り返さない文字で広がらないように。
    <div className="flex min-h-0 min-w-0 flex-1 flex-col gap-3">
      {/* 追加: 検索欄＋これから付ける権限 */}
      <div className="flex shrink-0 items-center gap-2">
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
              // IME の変換確定の Enter（isComposing）は日本語の入力操作なので、何もしない。
              if (e.nativeEvent.isComposing) return;
              if (e.key === "Enter") {
                e.preventDefault();
                (e.target as HTMLInputElement).blur();
              }
            }}
            placeholder={SEARCH_PLACEHOLDER}
            aria-label="共有する相手を検索"
            data-share-search=""
            className="pl-9 pr-9 max-md:h-11"
          />
          {searching ? (
            <Loader2
              className="absolute right-3 top-1/2 size-4 -translate-y-1/2 animate-spin text-muted-foreground"
              aria-label="検索中"
            />
          ) : null}
        </div>
        <RoleSelect
          value={addRole}
          onChange={setAddRole}
          disabled={anyPending}
          label="追加する相手の権限"
          size="lg"
        />
      </div>

      {/* 検索欄の下は 1 つの領域を入れ替える（検索語があれば候補、無ければ共有中の相手）。
          候補を相手一覧の上に差し込まないので、打鍵のたびに一覧が上下しない。 */}
      <div
        data-testid="share-people-scroll"
        className="-mx-1 min-h-0 flex-1 overflow-y-auto overscroll-contain px-1 pb-1"
      >
        {hasQuery ? (
          <div className="flex flex-col gap-3" aria-busy={searching || undefined}>
            {searching && results.length === 0 ? null : results.length === 0 ? (
              <Note icon={Search}>該当するメンバー・部署がありません</Note>
            ) : (
              <>
                {userResults.length > 0 ? (
                  <Section title="メンバー">
                    <ul className={PANEL} aria-label="メンバーの候補">
                      {userResults.map(renderCandidate)}
                    </ul>
                  </Section>
                ) : null}
                {roleResults.length > 0 ? (
                  <Section title="部署・ロール">
                    <ul className={PANEL} aria-label="部署・ロールの候補">
                      {roleResults.map(renderCandidate)}
                    </ul>
                  </Section>
                ) : null}
              </>
            )}
          </div>
        ) : (
          <Section title="共有中の相手" count={loadingShares ? null : shares.length}>
            {loadingShares ? (
              <div className="flex items-center gap-2 px-1 py-2 text-[13px] text-muted-foreground">
                <Loader2 className="size-4 animate-spin" aria-hidden />
                読み込み中…
              </div>
            ) : shares.length === 0 ? (
              <Note icon={Users}>
                まだ誰にも共有していません。
                <span className="mt-0.5 block text-xs">上の検索欄から、メンバーや部署を探して共有できます。</span>
              </Note>
            ) : (
              <ul className={PANEL}>
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
                    <li key={k} data-testid="share-entry" className={cn(ROW, "pr-1.5")}>
                      <PrincipalAvatar kind={s.target.type} name={name} />
                      <div className="min-w-0 flex-1">
                        <p className="truncate text-[13.5px] font-medium">{name ?? fallback}</p>
                        <p
                          className="truncate text-xs text-muted-foreground"
                          title={shortId ? s.target.id : undefined}
                        >
                          {secondary}
                        </p>
                      </div>
                      <RoleSelect
                        value={s.role}
                        onChange={(r) => void changeRole(s, r)}
                        disabled={anyPending}
                        label={`${a11yName} の権限`}
                      />
                      <button
                        type="button"
                        aria-label={`${a11yName} の共有を解除`}
                        title="共有を解除"
                        disabled={anyPending}
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
          </Section>
        )}
      </div>
    </div>
  );
}

/// 一覧の面と行（設定画面と同じシキの作法: 角の大きい面・行の間は控えめな荒い破線）。
const PANEL = "rule-soft overflow-hidden rounded-xl border bg-card";
const ROW = "shiki-dash-top flex items-center gap-3 px-3.5 py-2.5 first:bg-none";

/// 見出し（面の外に置く）＋中身。
function Section({
  title,
  count,
  children,
}: {
  title: string;
  count?: number | null;
  children: React.ReactNode;
}) {
  return (
    <section className="flex flex-col gap-2">
      <h3 className="flex items-baseline gap-2 px-1 text-[13px] font-semibold tracking-[-0.01em]">
        {title}
        {count ? <span className="text-xs font-normal tabular-nums text-muted-foreground">{count} 件</span> : null}
      </h3>
      {children}
    </section>
  );
}

/// 空・該当なしの一言（破線の枠。設定画面の「準備中」の一言と同じ形）。
function Note({ icon: Icon, children }: { icon: LucideIcon; children: React.ReactNode }) {
  return (
    <div className="flex items-center gap-2.5 rounded-xl border border-dashed px-4 py-3 text-[13px] text-muted-foreground">
      <Icon className="size-4 shrink-0" aria-hidden />
      <p className="min-w-0">{children}</p>
    </div>
  );
}

/// 相手のアバター。メンバーは表示名の頭文字（管理コンソールのメンバー一覧と同じ）、部署・ロールは人の集まりのアイコン。
function PrincipalAvatar({ kind, name }: { kind: PrincipalKind; name?: string }) {
  const initial = name?.trim().charAt(0);
  if (kind === "role") {
    // 部署・ロールは複数人に届くので、夏（新緑）の差し色で人と見分ける。
    return (
      <span
        aria-hidden
        style={seasonAccentStyle(GROUP_SEASON)}
        className="flex size-9 shrink-0 items-center justify-center rounded-full bg-[var(--season)]/[0.16] text-[var(--season)]"
      >
        <Users className="size-4" />
      </span>
    );
  }
  return (
    <span
      aria-hidden
      className="flex size-9 shrink-0 items-center justify-center rounded-full bg-muted text-[13px] font-semibold text-foreground/80"
    >
      {initial ? initial.toUpperCase() : <Users className="size-4" />}
    </span>
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
        // 権限の色（閲覧＝冬・編集＝秋）を枠と面にごく薄く敷き、一覧でも権限が一目で分かるようにする。
        style={seasonAccentStyle(ROLE_SEASON[value])}
        className={cn(
          "shrink-0 gap-1 border-[var(--season)]/40 bg-[var(--season)]/[0.08]",
          size === "lg"
            ? "h-9 w-[6rem] px-3 text-sm max-md:h-11 pointer-coarse:h-11"
            : "h-8 w-[5.25rem] px-2 text-xs pointer-coarse:h-9 pointer-coarse:text-[13px]",
        )}
      >
        <SelectValue />
      </SelectTrigger>
      <SelectContent align="end" className="min-w-[7rem]">
        <SelectItem value="viewer">
          <span className="flex items-center gap-1.5">
            <RoleDot role="viewer" />
            閲覧
          </span>
        </SelectItem>
        <SelectItem value="editor">
          <span className="flex items-center gap-1.5">
            <RoleDot role="editor" />
            編集
          </span>
        </SelectItem>
      </SelectContent>
    </Select>
  );
}
