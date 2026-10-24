import { DriveTabs } from "@/components/drive/drive-tabs";

/// ドライブ配下の共通レイアウト。スマホは本文の上端に区画のタブを出す（PC はサイドバー）。
/// レイアウトに置くのはタブを切り替えても再マウントさせず、読み込み中（loading.tsx）も残すため。
export default function DriveLayout({ children }: { children: React.ReactNode }) {
  return (
    <>
      <DriveTabs />
      {children}
    </>
  );
}
