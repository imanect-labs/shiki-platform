import { redirect } from "next/navigation";

/// /settings は最初の項目（プロフィール）へ。
export default function SettingsIndexPage() {
  redirect("/settings/profile");
}
