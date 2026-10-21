import type { Metadata } from "next";

import { SessionsView } from "@/components/settings/sessions-view";

export const metadata: Metadata = { title: "端末とセッション" };

export default function Page() {
  return <SessionsView />;
}
