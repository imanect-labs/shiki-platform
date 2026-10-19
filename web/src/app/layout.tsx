import type { Metadata, Viewport } from "next";
import type { ReactNode } from "react";

import { Providers } from "./providers";
import "./globals.css";

export const metadata: Metadata = {
  title: { default: "Shiki", template: "%s · Shiki" },
  description: "権限考慮 RAG・自律エージェント・ミニアプリ基盤を備えるAIプラットフォーム",
};

export const viewport: Viewport = {
  // キーボード表示時にレイアウトビューポートごと縮める（Android Chrome・下端の入力欄を隠さない）。
  interactiveWidget: "resizes-content",
  // ノッチ・ホームバーの内側まで描き、各コンポーネントが env(safe-area-inset-*) で避ける。
  viewportFit: "cover",
  themeColor: [
    { media: "(prefers-color-scheme: light)", color: "#ffffff" },
    { media: "(prefers-color-scheme: dark)", color: "#1a1a1c" },
  ],
};

export default function RootLayout({ children }: { children: ReactNode }) {
  // suppressHydrationWarning: next-themes が <html> の class を hydration 前に
  // 書き換えるため、サーバ/クライアント差分の警告を抑止する（公式推奨）。
  return (
    <html lang="ja" suppressHydrationWarning>
      <body className="font-sans">
        <Providers>{children}</Providers>
      </body>
    </html>
  );
}
