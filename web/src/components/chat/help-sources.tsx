import type { HelpCitation } from "@/lib/chat-api";

export function HelpSources({ citations }: { citations: HelpCitation[] }) {
  const unique = [...new Map(citations.map((c) => [c.cite_id, c])).values()];
  if (!unique.length) return null;
  return <section aria-label="ヘルプの出典" className="mt-3 space-y-2 text-sm">
    {unique.map((c) => <article key={c.cite_id} className="rounded-lg border p-3">
      <div className="font-medium">[H{c.cite_id}] {c.title}</div>
      <div className="text-muted-foreground">{c.heading_path.join(" › ")}</div>
      <p className="my-2 whitespace-pre-wrap">{c.snippet}</p>
      <a href={c.url} target="_blank" rel="noopener noreferrer" className="text-primary underline">ヘルプで開く</a>
    </article>)}
  </section>;
}
