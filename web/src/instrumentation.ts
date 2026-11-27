/// Next.js 標準の計装フック。中身は `instrumentation-node.ts`（OpenTelemetry・nodejs ランタイムだけ）。
///
/// `NEXT_RUNTIME` の比較は**ビルド時に定数へ置き換わる**ので、この `if` の中で import すると edge（middleware）の
/// バンドルから OTel が落ちる（早期 return の形では落ちず、middleware が 32KB → 93KB に膨らんだ）。
/// OTLP の宛先が空なら OTel のコード（サーバ側の約 300KB のチャンク）を読み込みもしない。
export async function register(): Promise<void> {
  if (process.env.NEXT_RUNTIME === "nodejs") {
    // 空白だけの値は未設定として扱う。トレース専用の宛先が空白だけなら、共通の宛先に任せる
    // （@vercel/otel は専用の値を先に読むので、空白のまま残すと共通の宛先が使われない）。
    if (!process.env.OTEL_EXPORTER_OTLP_TRACES_ENDPOINT?.trim()) delete process.env.OTEL_EXPORTER_OTLP_TRACES_ENDPOINT;
    const tracesEndpoint = process.env.OTEL_EXPORTER_OTLP_TRACES_ENDPOINT?.trim();
    const endpoint = process.env.OTEL_EXPORTER_OTLP_ENDPOINT?.trim();
    // OTel の仕様どおり、トレース専用の宛先だけでも有効にする。
    if (!tracesEndpoint && !endpoint) return;
    const { registerNodeOTel } = await import("./instrumentation-node");
    registerNodeOTel();
  }
}
