/// Next.js サーバ（nodejs ランタイム）の OpenTelemetry 計装。`instrumentation.ts` の `register` から読み込む。
///
/// - `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` か `OTEL_EXPORTER_OTLP_ENDPOINT` が空なら**何もしない**（エクスポートしない・
///   fetch も差し替えない＝計装前と同じ動き）。宛先は OTLP/HTTP（collector の 4318）。
/// - BFF の中継（`/api/*` の route handler）が shiki-server に送る fetch に W3C `traceparent` を付け、shiki-server の
///   リクエスト span が Next.js の span の下につながるようにする。付けるのは `BACKEND_ORIGIN` 宛てだけ（外部には
///   トレースの識別子を漏らさない）。ブラウザが `traceparent` を送ってきた場合、Next.js はその trace を継続し、
///   上流へ送る `traceparent` の親は Next.js の fetch span に置き換わる。
/// - next.config.mjs の rewrites（`/auth/*` の素通し・共同編集の WebSocket）は route handler を通らないので、
///   Next.js のリクエスト span の下には入らない。内部プロキシの送信（node:http）にこの計装が `http GET …` の span を
///   張り、その span を親とする `traceparent` が付く（Next.js のリクエスト span とは別のトレースになる）。
/// - ブラウザ側の計装は対象外。
import { registerOTel } from "@vercel/otel";

/// 呼ぶ側（`instrumentation.ts`）が OTLP の宛先の有無を確かめてから呼ぶ。
export function registerNodeOTel(): void {
  const backend = new URL(process.env.BACKEND_ORIGIN ?? "http://localhost:8080").origin;
  registerOTel({
    serviceName: process.env.OTEL_SERVICE_NAME || "shiki-web",
    instrumentationConfig: {
      // 前方一致で比べるので、末尾の `/` まで含めて別ポート・別ホストの取り違えを防ぐ。
      fetch: { propagateContextUrls: [`${backend}/`] },
    },
  });
}
