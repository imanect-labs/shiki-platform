"use client";

import * as DialogPrimitive from "@radix-ui/react-dialog";
import { VisuallyHidden } from "@radix-ui/react-visually-hidden";

import { cn } from "@/lib/utils";
import { useSidebar } from "./sidebar/sidebar-context";
import { SidebarContent } from "./sidebar/sidebar";

/// モバイル（< md）でのサイドバー。左からスライドするドロワ。
/// Radix Dialog でフォーカストラップ・Esc・スクロールロック・aria-modal を担保する。
export function MobileSidebar() {
  const { mobileOpen, setMobileOpen } = useSidebar();

  return (
    <DialogPrimitive.Root open={mobileOpen} onOpenChange={setMobileOpen}>
      <DialogPrimitive.Portal>
        <DialogPrimitive.Overlay
          className={cn(
            "fixed inset-0 z-50 bg-black/40 backdrop-blur-[1px] md:hidden",
            "data-[state=open]:animate-in data-[state=open]:fade-in-0 data-[state=closed]:animate-out data-[state=closed]:fade-out-0",
          )}
        />
        <DialogPrimitive.Content
          className={cn(
            "fixed inset-y-0 left-0 z-50 w-[min(20rem,85vw)] border-r border-sidebar-border shadow-lg outline-none md:hidden",
            "data-[state=open]:animate-in data-[state=open]:slide-in-from-left data-[state=closed]:animate-out data-[state=closed]:slide-out-to-left",
            "duration-200",
          )}
        >
          <VisuallyHidden>
            <DialogPrimitive.Title>ナビゲーション</DialogPrimitive.Title>
            <DialogPrimitive.Description>
              サイドバーのメニュー
            </DialogPrimitive.Description>
          </VisuallyHidden>
          {/* モバイルは常に展開・折りたたみトグル無し・遷移で自動クローズ */}
          <SidebarContent
            collapsed={false}
            showCollapseToggle={false}
            onNavigate={() => setMobileOpen(false)}
          />
        </DialogPrimitive.Content>
      </DialogPrimitive.Portal>
    </DialogPrimitive.Root>
  );
}
