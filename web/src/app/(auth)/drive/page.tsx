import type { Metadata } from "next";
import { Suspense } from "react";

import { DriveBrowser } from "@/components/drive/drive-browser";
import { LoadingRow } from "@/components/drive/primitives";
import { PageContainer } from "@/components/shell/page-container";

export const metadata: Metadata = { title: "ドライブ" };

/// ドライブのホーム。フォルダブラウズ／アップロード／共有／版履歴（#20 / Task 1.10）。
/// `useSearchParams`（現在フォルダ）を含むため Suspense 境界で包む。
export default function DriveHomePage() {
  return (
    <PageContainer title="ドライブ" description="ファイルとフォルダを管理します。">
      <Suspense fallback={<LoadingRow />}>
        <DriveBrowser />
      </Suspense>
    </PageContainer>
  );
}
