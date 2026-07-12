// esbuild の最小型スタブ（CLI は動的 import で build のみ使用・実体は npm の esbuild）。
declare module "esbuild" {
  export function build(options: unknown): Promise<{ outputFiles: { text: string }[] }>;
}
