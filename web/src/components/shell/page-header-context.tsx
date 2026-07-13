"use client";

/// ページ ヘッダ スロット。共通シェルの `<Header>`（唯一の h-14 バー）へ、各ページが
/// タイトル/アクションを注入するための仕組み。これにより「シェルのバー＋ページ自前のバー」の
/// 横バー二重（/c・notes・csv・workflow）を解消し、ヘッダを 1 本に統一する。
///
/// 使い方（ページ側・"use client"）:
///   usePageHeader(() => (
///     <PageHeaderBar title="会話">
///       <ShareButton/>
///     </PageHeaderBar>
///   ), [依存]);
///
/// ページがアンマウントするとスロットは空に戻り、Header はルート由来の既定表示に戻る。

import * as React from "react";

import { cn } from "@/lib/utils";

type SetHeader = (node: React.ReactNode | null) => void;

const PageHeaderContext = React.createContext<SetHeader | null>(null);
const PageHeaderValueContext = React.createContext<React.ReactNode>(null);

export function PageHeaderProvider({ children }: { children: React.ReactNode }) {
  const [node, setNode] = React.useState<React.ReactNode>(null);
  return (
    <PageHeaderContext.Provider value={setNode}>
      <PageHeaderValueContext.Provider value={node}>{children}</PageHeaderValueContext.Provider>
    </PageHeaderContext.Provider>
  );
}

/// Header が読む: 現在ページが注入したヘッダ内容（無ければ null）。
export function usePageHeaderValue(): React.ReactNode {
  return React.useContext(PageHeaderValueContext);
}

/// ページ側: ヘッダ内容を注入する。`render` は依存が変わるたびに再評価され、
/// アンマウントで自動的にクリアする。依存には title やハンドラの元になる state を渡す。
export function usePageHeader(render: () => React.ReactNode, deps: React.DependencyList): void {
  const setHeader = React.useContext(PageHeaderContext);
  React.useEffect(() => {
    if (!setHeader) return;
    setHeader(render());
    return () => setHeader(null);
    // render は factory。依存は呼び出し側が明示する（title/handlers の元）。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [setHeader, ...deps]);
}

/// ヘッダ内の共通レイアウト: 左にタイトル、右（ml-auto）にアクション群。
/// 各ページで同じ体裁に揃えるための presentational ヘルパ。
export function PageHeaderBar({
  title,
  children,
  className,
}: {
  title: React.ReactNode;
  /// 右寄せのアクション（ボタン/メニュー）。
  children?: React.ReactNode;
  className?: string;
}) {
  return (
    <div className={cn("flex min-w-0 flex-1 items-center gap-2", className)}>
      <span className="min-w-0 flex-1 truncate text-sm font-medium text-foreground">{title}</span>
      {children ? <div className="flex shrink-0 items-center gap-1">{children}</div> : null}
    </div>
  );
}
