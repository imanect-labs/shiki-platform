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
import type { Span, SpanProcessor } from "@opentelemetry/sdk-trace-base";
import { registerOTel } from "@vercel/otel";

/// 呼ぶ側（`instrumentation.ts`）が OTLP の宛先の有無を確かめてから呼ぶ。
export function registerNodeOTel(): void {
  const backend = backendOrigin(process.env.BACKEND_ORIGIN);
  registerOTel({
    serviceName: process.env.OTEL_SERVICE_NAME || "shiki-web",
    // 既定の処理（エクスポート）の前に、URL のクエリを span から落とす。
    spanProcessors: [new StripQuerySpanProcessor(), "auto"],
    instrumentationConfig: {
      // 前方一致で比べるので、末尾の `/` まで含めて別ポート・別ホストの取り違えを防ぐ。
      // 宛先が読めないときは、どこにも付けない（span のエクスポートは続ける）。
      fetch: { propagateContextUrls: backend ? [`${backend}/`] : [] },
    },
  });
}

/// `BACKEND_ORIGIN` の origin。中継（`app/api/**/route.ts`）と同じく、未設定なら既定の `http://localhost:8080`。
/// 空（空白だけ）なら中継先が無いので、どこにも付けない。
/// URL として読めない値でもサーバの起動は止めない（計装は観測のための付け足しなので）。
function backendOrigin(raw: string | undefined): string | null {
  if (raw === undefined) return "http://localhost:8080";
  if (!raw.trim()) return null;
  try {
    return new URL(raw.trim()).origin;
  } catch {
    console.warn("[instrumentation] BACKEND_ORIGIN を URL として読めません。traceparent は付けません。");
    return null;
  }
}

/// URL を持つ属性。値のクエリ（`?` 以降）を落とす。
const URL_ATTRIBUTES = ["http.url", "http.target", "url.full"] as const;

/// span の名前と URL 属性からクエリ文字列を落とす。
///
/// クエリには OAuth の認可コード（`/auth/callback?code=…`）や検索語など、トレースに残してはいけない値が
/// 入る。Next.js の span（`http.target`）と、fetch・http の計装の span（名前と `http.url`）が対象。
/// 属性は span の開始時に載るので、開始時に書き換えれば、エクスポートされる前に消える。
export class StripQuerySpanProcessor implements SpanProcessor {
  onStart(span: Span): void {
    const name = stripQuery(span.name);
    if (name !== span.name) span.updateName(name);
    for (const key of URL_ATTRIBUTES) {
      const value = span.attributes[key];
      if (typeof value === "string" && value.includes("?")) span.setAttribute(key, stripQuery(value));
    }
    if (span.attributes["url.query"] !== undefined) span.setAttribute("url.query", "");
  }
  onEnd(): void {}
  forceFlush(): Promise<void> {
    return Promise.resolve();
  }
  shutdown(): Promise<void> {
    return Promise.resolve();
  }
}

/// `?` から次の空白の手前までを落とす（span 名は `fetch GET <URL>` の形なので、URL の後ろは残す）。
export function stripQuery(value: string): string {
  return value.replace(/\?[^\s]*/g, "");
}
