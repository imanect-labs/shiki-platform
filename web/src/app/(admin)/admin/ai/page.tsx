import { redirect } from "next/navigation";

/// /admin/ai は利用量へ（セクションの入口）。
export default function AdminAiPage() {
  redirect("/admin/ai/usage");
}
