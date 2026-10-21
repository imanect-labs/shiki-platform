import { cn } from "@/lib/utils";

/// 四つの点（ロゴマーク）。左上から時計回りに春・夏・秋・冬。色は四季トークンが正本なので、
/// ライト/ダークの切り替えはトークン側に任せる。点の位置と大きさは
/// deploy/docker/collabora/branding/core/*.svg・web/src/app/icon.svg と揃えている。
export function BrandMark({ className }: { className?: string }) {
  return (
    <svg aria-hidden viewBox="0 0 64 64" className={cn("size-5 shrink-0", className)}>
      <circle cx="21" cy="21" r="8.6" fill="var(--season-spring)" />
      <circle cx="43" cy="21" r="8.6" fill="var(--season-summer)" />
      <circle cx="43" cy="43" r="8.6" fill="var(--season-autumn)" />
      <circle cx="21" cy="43" r="8.6" fill="var(--season-winter)" />
    </svg>
  );
}

const LOGO_SIZES = {
  md: { gap: "gap-1.5", mark: "size-[22px]", text: "text-[17px]" },
  touch: { gap: "gap-2", mark: "size-6", text: "text-[19px]" },
  lg: { gap: "gap-2", mark: "size-8", text: "text-[26px]" },
} as const;

/// ロゴ（マーク＋ワードマーク）。色を持つのは点だけで、文字はネイビー一色（ダークは前景色）。
/// `touch` はスマホのドロワ用に一段大きくしたもの。
export function BrandLogo({
  size = "md",
  className,
}: {
  size?: keyof typeof LOGO_SIZES;
  className?: string;
}) {
  const s = LOGO_SIZES[size];
  return (
    <span className={cn("inline-flex items-center", s.gap, className)}>
      <BrandMark className={s.mark} />
      <span
        className={cn(
          "font-bold leading-none tracking-[-0.02em] text-primary dark:text-foreground",
          s.text,
        )}
      >
        Shiki
      </span>
    </span>
  );
}
