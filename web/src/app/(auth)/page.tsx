"use client";

import { useRouter } from "next/navigation";

import { useMe } from "@/hooks/use-me";
import { createSession } from "@/lib/chat-store";
import { Skeleton } from "@/components/ui/skeleton";
import { Composer } from "@/components/chat/composer";
import { ShortcutGrid } from "@/components/home/shortcut-grid";

/// ホーム＝新規チャットの起点（画像1 のワークスペース風）。
/// 中央のコンポーザに入力して送信するとセッションを作成し会話画面へ遷移する。
/// 下部にはロードマップ準拠の機能ショートカット枠を並べる。
export default function HomePage() {
  const router = useRouter();
  const { data, loading } = useMe();
  const name = data?.email?.split("@")[0] ?? null;

  const startChat = (text: string) => {
    const session = createSession(text);
    router.push(`/c/${session.id}`);
  };

  return (
    <div className="mx-auto flex min-h-full w-full max-w-3xl flex-col justify-center px-4 py-10">
      <div className="flex flex-col items-center gap-9">
        <div className="text-center">
          {loading ? (
            <Skeleton className="mx-auto h-9 w-72" />
          ) : (
            <h1 className="text-[28px] font-semibold tracking-tight text-foreground sm:text-[32px]">
              {name ? `${name} さん、こんにちは` : "Shiki へようこそ"}
            </h1>
          )}
          <p className="mt-2 text-sm text-muted-foreground">
            何でも尋ねて、何でも作成しましょう。
          </p>
        </div>

        <Composer onSubmit={startChat} autoFocus className="w-full max-w-2xl" />

        <ShortcutGrid />
      </div>
    </div>
  );
}
