/// Next.js 標準の計装フック。中身は `instrumentation-node.ts`（OpenTelemetry・nodejs ランタイムだけ）。
///
/// `NEXT_RUNTIME` の比較は**ビルド時に定数へ置き換わる**ので、この `if` の中で import すると edge（middleware）の
/// バンドルから OTel が落ちる（早期 return の形では落ちず、middleware が 32KB → 93KB に膨らんだ）。
/// OTLP の宛先が空なら OTel のコード（サーバ側の約 300KB のチャンク）を読み込みもしない。
export async function register(): Promise<void> {
  if (process.env.NEXT_RUNTIME === "nodejs") {
    const endpoint = process.env.OTEL_EXPORTER_OTLP_TRACES_ENDPOINT || process.env.OTEL_EXPORTER_OTLP_ENDPOINT || "";
    if (!endpoint.trim()) return;
    const { registerNodeOTel } = await import("./instrumentation-node");
    registerNodeOTel();
  }
}
