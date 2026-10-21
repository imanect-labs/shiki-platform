import type { Metadata } from "next";

import { ProfileView } from "@/components/settings/profile-view";

export const metadata: Metadata = { title: "プロフィール" };

export default function Page() {
  return <ProfileView />;
}
