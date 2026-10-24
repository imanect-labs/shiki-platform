"use client";

/// ドキュメントに紐づくチャットパネル（分割ビューのサイドパネル・汎用）。
///
/// ノート（Yjs meta にアクティブ会話 id）と Office 文書（localStorage にアクティブ会話 id）で
/// **アクティブ会話の保存先だけが違う**ため、そこを [`ActiveThreadStore`] に抽象化して共有する。
/// ドキュメント:会話 = 1:N（`origin_note_id` = ドキュメントの node id で会話一覧を引く）。
/// 会話 UI は既存 [`Conversation`]（variant=panel）を再利用する。
///
/// 会話は**最初の発話を送ったとき**に作る（[`DocChatDraft`]）。パネルを開いただけ・「新しい会話」を
/// 押しただけでは作らない（空の会話がサイドバーの履歴に並ばないように）。

import { Check, ChevronDown, Loader2, MessagesSquare, Plus } from "lucide-react";
import * as React from "react";

import { Conversation } from "@/components/chat/conversation";
import { DocChatDraft, type FirstMessage } from "@/components/chat/doc-chat-draft";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { createThread, getThread, listThreads, postMessage, type Thread } from "@/lib/chat-api";
import { moveModelChoice, readModelChoice } from "@/lib/model-choice";
import { readToolChoice, settleToolChoice } from "@/lib/tool-choice";
import {
  readScoped,
  storageScopeGeneration,
  subscribeStorageScope,
  writeScoped,
} from "@/lib/user-storage";
import { cn } from "@/lib/utils";

/// アクティブ会話 id の保存先の抽象（ノート=Yjs meta・Office=localStorage）。
export interface ActiveThreadStore {
  read: () => string | null;
  write: (id: string) => void;
  /// 外部変更（別共同編集者の切替等）を購読する。無い保存先は no-op を返してよい。
  subscribe: (onChange: () => void) => () => void;
}

export function DocChatPanel({
  store,
  nodeId,
  title,
  label,
  editable,
  initialThreadId,
  testIdPrefix = "doc",
}: {
  store: ActiveThreadStore;
  /// ドキュメントのストレージ node id（origin_note_id に使う）。
  nodeId: string;
  /// 会話タイトルの接頭（例「ノート: 提案書」）。
  title: string;
  /// 一覧見出しの語（例「このノートの会話」「この文書の会話」）。
  label: string;
  /// editor のみが会話を新規作成できる（viewer は既存会話の閲覧のみ）。
  editable: boolean;
  /// サイドバー等から特定会話を開くときの初期アクティブ（?thread=）。
  initialThreadId?: string | null;
  /// data-testid の接頭（既存 e2e 互換: ノート="note"・Office="office"）。
  testIdPrefix?: string;
}) {
  const [activeThreadId, setActiveThreadIdState] = React.useState<string | null>(() =>
    store.read(),
  );
  // ?thread= の会話がこの文書のものか確かめている間（確かめ終わるまで入力欄を出さない）。
  const [verifyingInitial, setVerifyingInitial] = React.useState(Boolean(initialThreadId));
  const [conversations, setConversations] = React.useState<Thread[]>([]);
  // 「新しい会話」を押して、まだ最初の発話を送っていない間（アクティブ会話はそのまま残す）。
  const [drafting, setDrafting] = React.useState(false);
  // 会話を作る前の入力欄を出しているか・そこに書きかけがあるか（外部からの切り替えで消さないため）。
  const showingDraftRef = React.useRef(false);
  const draftDirtyRef = React.useRef(false);
  // 会話の選択の版。最初の発話を送る間に会話が切り替わったら（自分の選択・ディープリンク・
  // 共同編集者）、送り終えても表示を奪わない。
  const selectionVersionRef = React.useRef(0);
  // 作ったが最初の発話を送れなかった会話（送り直しで使い回し、空の会話を増やさない）。
  // 別の会話へ切り替えたら捨てる（その会話が一覧から開かれて使われ得るため）。
  const unsentThreadRef = React.useRef<{ nodeId: string; id: string; generation: number } | null>(
    null,
  );
  const onDraftDirtyChange = React.useCallback((dirty: boolean) => {
    draftDirtyRef.current = dirty;
  }, []);

  // 非同期処理（会話の作成・確認）の完了で選ぶときは、開始時の世代（`startedAt`）を渡す。
  // 待つ間にログアウト（他タブを含む）があれば、前の人の会話 id を次の人のキーへ書かない（#817）。
  const setActive = React.useCallback(
    (id: string, startedAt?: number) => {
      if (startedAt !== undefined && startedAt !== storageScopeGeneration()) return;
      selectionVersionRef.current += 1;
      unsentThreadRef.current = null;
      store.write(id);
      setActiveThreadIdState(id);
    },
    [store],
  );

  // 外部変更（別クライアントの切替）に追従する。
  React.useEffect(() => {
    return store.subscribe(() => {
      // 会話の無い状態で書きかけている間に、共同編集者が最初の会話を作った（ノートはアクティブ
      // 会話を共有する）。入力欄を相手の会話に置き換えると本文が消えるので、新しい会話の入力欄
      // として残す（相手の会話は切り替えから開ける）。
      if (showingDraftRef.current && draftDirtyRef.current) setDrafting(true);
      selectionVersionRef.current += 1;
      unsentThreadRef.current = null;
      setActiveThreadIdState(store.read());
    });
  }, [store]);

  // ?thread= で来た会話をアクティブへ（値が変わるたび反映・deep link 切替対応）。
  // URL は書き換えられるので、**この文書の会話であること**を確かめてから適用する。別文書や
  // 通常チャットの会話を指定されても、このパネルに混ぜず、保存済みの選択も上書きしない（#598）。
  const appliedInitialRef = React.useRef<string | null>(null);
  React.useEffect(() => {
    if (!initialThreadId) {
      // ディープリンクが外れた（確かめている途中に同じ文書のクエリなし URL へ移った等）。
      // 確認待ちのまま残すと、入力欄が出ず「準備中」から進まない。
      setVerifyingInitial(false);
      return;
    }
    if (appliedInitialRef.current === initialThreadId) return;
    appliedInitialRef.current = initialThreadId;
    let cancelled = false;
    setVerifyingInitial(true);
    const startedAt = storageScopeGeneration();
    getThread(initialThreadId)
      .then((t) => {
        if (!cancelled && t.originNoteId === nodeId) {
          if (t.id !== store.read()) setActive(t.id, startedAt);
          setDrafting(false);
        }
      })
      .catch(() => {
        /* 見えない・存在しない会話は適用しない（保存済みの選択のまま）。 */
      })
      .finally(() => {
        if (!cancelled) setVerifyingInitial(false);
      });
    return () => {
      // 確かめ終わる前に外れたら（StrictMode の二重実行・依存の変化）、次の実行でやり直す。
      cancelled = true;
      appliedInitialRef.current = null;
    };
  }, [initialThreadId, nodeId, store, setActive]);

  // このドキュメントの会話一覧を origin_note_id で引く。アクティブが一覧に無ければ補う。
  const refreshConversations = React.useCallback(async () => {
    try {
      const { threads } = await listThreads(undefined, { originNoteId: nodeId });
      let list = threads;
      const active = store.read();
      if (active && !list.some((t) => t.id === active)) {
        const t = await getThread(active).catch(() => null);
        // 一覧の 1 ページ目に無い古い会話を補う。別文書の会話は混ぜない。
        if (t && t.originNoteId === nodeId) list = [t, ...list];
      }
      list.sort((a, b) => a.createdAt.localeCompare(b.createdAt));
      setConversations(list);
    } catch {
      /* 一覧取得失敗は致命的ではない（アクティブ会話は表示できる）。 */
    }
  }, [nodeId, store]);

  React.useEffect(() => {
    void refreshConversations();
  }, [refreshConversations, activeThreadId]);

  // 会話を作る前の入力欄のモデル・ツールの選択の保存先（ホームや他の文書の入力欄と分ける）。
  const draftScope = `docdraft:${nodeId}`;

  // 最初の発話で会話を作り、**ここで送る**（会話側のマウントに頼らない＝作成を待つ間に
  // パネルを閉じても発話は届く）。会話側は進行中の run として読み込んで表示する（再訪と同じ）。
  const startConversation = React.useCallback(
    async (msg: FirstMessage) => {
      const startedAt = storageScopeGeneration();
      const selection = selectionVersionRef.current;
      const unsent = unsentThreadRef.current;
      unsentThreadRef.current = null;
      let threadId =
        unsent?.nodeId === nodeId && unsent.generation === startedAt ? unsent.id : null;
      if (!threadId) {
        const n = conversations.length + 1;
        const thread = await createThread(n <= 1 ? title : `${title} (${n})`, msg.autonomous, {
          originNoteId: nodeId,
        });
        threadId = thread.id;
      }
      // 待つ間にログアウト（他タブを含む）があれば、前の人の発話・選択を送らない・移さない（#817）。
      if (startedAt !== storageScopeGeneration()) return;
      const tools = readToolChoice(draftScope);
      try {
        await postMessage(threadId, msg.text, msg.attachments, {
          agentMode: msg.autonomous,
          autonomous: msg.autonomous,
          context: msg.context,
          skills: msg.skills,
          model: readModelChoice(draftScope),
          tools,
        });
      } catch (e) {
        unsentThreadRef.current = { nodeId, id: threadId, generation: startedAt };
        throw e;
      }
      // ツールはこの発話にだけ効く（受理されたら空に戻す）。モデルは会話の続きでも使う。
      settleToolChoice(draftScope, tools);
      if (startedAt !== storageScopeGeneration()) return;
      moveModelChoice(draftScope, threadId);
      if (selection !== selectionVersionRef.current) {
        void refreshConversations();
        return;
      }
      // 一覧はアクティブの変化で読み直される（上の effect）。
      setActive(threadId, startedAt);
      setDrafting(false);
    },
    [conversations.length, title, nodeId, draftScope, setActive, refreshConversations],
  );

  const selectConversation = React.useCallback(
    (id: string) => {
      selectionVersionRef.current += 1;
      setDrafting(false);
      if (id !== activeThreadId) setActive(id);
    },
    [activeThreadId, setActive],
  );

  // 表示中の会話（「新しい会話」の入力中は無し＝入力欄を出す）。
  const shownThreadId = drafting ? null : activeThreadId;
  showingDraftRef.current = shownThreadId === null;
  const activeIndex = shownThreadId ? conversations.findIndex((c) => c.id === shownThreadId) : -1;

  return (
    <div
      className="flex h-full min-h-0 w-full flex-col"
      aria-label="ドキュメントのチャット"
      data-testid={`${testIdPrefix}-chat-panel`}
    >
      {/* 切り替え先がある（会話を 1 つ以上持つ）ときだけ出す。初めての会話は入力欄だけ。 */}
      {activeThreadId || conversations.length > 0 ? (
        <div
          className="flex shrink-0 items-center gap-1 px-2 py-1.5 shiki-dash-bottom"
          data-testid={`${testIdPrefix}-conversation-switcher`}
        >
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <button
                type="button"
                className="inline-flex min-w-0 items-center gap-1.5 rounded-md px-2 py-1 text-[13px] text-foreground/80 outline-none transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring pointer-coarse:min-h-10"
                title="会話を切り替える"
              >
                <MessagesSquare className="size-3.5 shrink-0 text-muted-foreground" aria-hidden />
                <span className="truncate">
                  {shownThreadId
                    ? activeIndex >= 0
                      ? `会話 ${activeIndex + 1}`
                      : "会話"
                    : "新しい会話"}
                </span>
                {conversations.length > 1 ? (
                  <span className="rounded-full bg-muted px-1.5 text-[11px] text-muted-foreground">
                    {conversations.length}
                  </span>
                ) : null}
                <ChevronDown className="size-3.5 shrink-0 text-muted-foreground" aria-hidden />
              </button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="start" className="w-56">
              <DropdownMenuLabel>{label}</DropdownMenuLabel>
              {conversations.map((c, i) => (
                <DropdownMenuItem key={c.id} onClick={() => selectConversation(c.id)} className="gap-2">
                  <Check
                    className={cn(
                      "size-3.5 shrink-0",
                      c.id === shownThreadId ? "opacity-100" : "opacity-0",
                    )}
                    aria-hidden
                  />
                  <span className="flex-1 truncate">会話 {i + 1}</span>
                  <span className="shrink-0 text-[11px] text-muted-foreground">
                    {new Date(c.updatedAt).toLocaleDateString("ja-JP", {
                      month: "numeric",
                      day: "numeric",
                    })}
                  </span>
                </DropdownMenuItem>
              ))}
            </DropdownMenuContent>
          </DropdownMenu>
          <span className="flex-1" />
          {/* 新しい会話の入力欄を出している間は出さない（左の表示と重なり、押しても何も起きない）。 */}
          {editable && shownThreadId ? (
            <button
              type="button"
              onClick={() => setDrafting(true)}
              data-testid={`${testIdPrefix}-new-conversation`}
              className="inline-flex shrink-0 items-center gap-1 rounded-md px-2 py-1 text-[12px] text-muted-foreground outline-none transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-50 pointer-coarse:min-h-10 pointer-coarse:text-[13px]"
              title="新しい会話を開始（この会話は履歴に残ります）"
            >
              <Plus className="size-3.5" aria-hidden />
              新しい会話
            </button>
          ) : null}
        </div>
      ) : null}

      <div className="min-h-0 flex-1">
        {shownThreadId ? (
          <Conversation key={shownThreadId} threadId={shownThreadId} variant="panel" />
        ) : verifyingInitial ? (
          <div className="flex h-full items-center justify-center gap-2 text-sm text-muted-foreground">
            <Loader2 className="size-4 animate-spin" aria-hidden />
            チャットを準備しています…
          </div>
        ) : editable ? (
          <DocChatDraft
            scope={draftScope}
            onStart={startConversation}
            onDirtyChange={onDraftDirtyChange}
          />
        ) : (
          <div className="flex h-full items-center justify-center px-4 text-center text-sm text-muted-foreground">
            まだチャットが開始されていません。
          </div>
        )}
      </div>
    </div>
  );
}

/// localStorage をアクティブ会話 id の保存先にする [`ActiveThreadStore`]（Office 文書用・
/// Yjs を持たないドキュメントの per-user 会話継続）。キーはユーザー＋テナント単位で、
/// ログアウトで消える（[`@/lib/user-storage`]・#817）。
export function useLocalActiveThreadStore(key: string): ActiveThreadStore {
  return React.useMemo<ActiveThreadStore>(() => {
    const storageKey = `docchat:active:${key}`;
    return {
      read: () => {
        try {
          return readScoped(storageKey);
        } catch {
          return null;
        }
      },
      write: (id) => {
        try {
          writeScoped(storageKey, id);
        } catch {
          /* プライベートモード等では保存できない（セッション内は state が保持する） */
        }
      },
      // localStorage は同一タブ内の setItem では storage イベントが出ない。ユーザーの確定
      // （/me 応答）だけを購読し、確定前に読んだ空の値をそのユーザーの保存値で読み直す。
      // 保存値が無い変化（ログアウトでスコープが解けた等）は通知しない（アクティブを null に
      // 戻すと「会話が無い」とみなして、表示中の会話を新しい会話の入力欄に置き換えるため）。
      subscribe: (onChange) =>
        subscribeStorageScope(() => {
          try {
            if (readScoped(storageKey) !== null) onChange();
          } catch {
            /* 読めない環境では追従しない */
          }
        }),
    };
  }, [key]);
}
