import type { Metadata } from "next";
import { MockDirectoryView } from "@/components/admin/mock-directory-view";
export const metadata: Metadata = { title: "AD連携" };
export default function DirectoryPage() { return <MockDirectoryView />; }
