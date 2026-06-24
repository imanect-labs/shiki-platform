"use client";

import { LogIn } from "lucide-react";

import { login } from "@/lib/auth";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";

/// ログイン画面の最小プレースホルダ。本実装は #68（Task 1.13）。
/// シェル外（(auth) の外）なのでサイドバーは付かないが、トークンは共有する。
export default function LoginPage() {
  return (
    <div className="flex min-h-dvh items-center justify-center bg-background px-4">
      <Card className="w-full max-w-sm">
        <CardHeader className="items-center text-center">
          <span className="mb-1 text-[22px] font-bold tracking-[-0.02em] text-foreground">Shiki</span>
          <CardTitle className="text-xl">Shiki にログイン</CardTitle>
          <CardDescription>組織アカウント（Keycloak）で続行します。</CardDescription>
        </CardHeader>
        <CardContent>
          <Button className="w-full" onClick={() => login()}>
            <LogIn className="size-4" aria-hidden />
            Keycloak でログイン
          </Button>
        </CardContent>
      </Card>
    </div>
  );
}
