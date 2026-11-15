import type { Metadata } from "next";
import { DirectoryView } from "@/components/admin/directory-live/view";
export const metadata: Metadata = { title: "AD連携" };
export default function DirectoryPage() { return <DirectoryView />; }
