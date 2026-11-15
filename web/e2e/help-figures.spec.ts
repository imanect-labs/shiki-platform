import { test, expect } from "@playwright/test";
import type { HelpCitation } from "../src/lib/chat-api";
import { placeHelpFigures } from "../src/lib/help-citation";

// ヘルプの図の差し込み（純粋関数・ブラウザは使わない）。モデルが図を入れなかった回でも、
// 引いた手順の下に図が出ることを確かめる。

const FIG1 = "https://shiki.imanect.app/help/media/images/password--start.png";
const FIG2 = "https://shiki.imanect.app/help/media/images/password--configure.png";

function cite(cite_id: number, images: { url: string; alt: string }[] = []): HelpCitation {
  return {
    type: "help_citation",
    cite_id,
    chunk_id: `chunk-${cite_id}`,
    corpus_version: "test",
    title: "パスワードを設定・変更する",
    heading_path: ["手順"],
    url: "https://shiki.imanect.app/help/account/security/password/#steps",
    snippet: "",
    images,
    app_links: [],
  };
}

test("番号付きの手順で最初に引いた項目の下に、字下げして図を足す", () => {
  const text = [
    "パスワードは個人設定から変更できます [H1]。",
    "",
    "1. ログイン方法を開きます [H2]。",
    "2. 「パスワードを変更」を押します [H1]。",
    "   画面の①です。",
    "3. 新しいパスワードを入れます。",
  ].join("\n");
  const [out] = placeHelpFigures([text], [
    cite(1, [
      { url: FIG1, alt: "① 変更 [ボタン]" },
      { url: FIG2, alt: "入力欄" },
    ]),
    cite(2),
  ]);
  expect(out.split("\n")).toEqual([
    "パスワードは個人設定から変更できます [H1]。",
    "",
    "1. ログイン方法を開きます [H2]。",
    "2. 「パスワードを変更」を押します [H1]。",
    "   画面の①です。",
    `   ![① 変更  ボタン](${FIG1})`,
    `   ![入力欄](${FIG2})`,
    "3. 新しいパスワードを入れます。",
  ]);
});

test("モデルが本文に入れた図は足さない。手順が無ければ段落の後ろに置く", () => {
  const text = `設定画面から変えられます [H1]。\n続けて保存します。\n\n![図](${FIG1})`;
  const [out] = placeHelpFigures([text], [cite(1, [{ url: FIG1, alt: "a" }, { url: FIG2, alt: "b" }])]);
  expect(out).toBe(`設定画面から変えられます [H1]。\n続けて保存します。\n\n![b](${FIG2})\n\n![図](${FIG1})`);
});

test("引いていない出典・表・コードの中の番号には足さず、上限を守り、区切りをまたいで 1 度だけ出す", () => {
  const figs = (n: number) =>
    Array.from({ length: n }, (_, i) => ({ url: `https://shiki.imanect.app/help/media/images/f-${i}.png`, alt: `${i}` }));
  const texts = [
    "調べます。",
    "| 項目 | 説明 |\n| --- | --- |\n| A | [H1] |\n\n```\n[H1]\n```",
    "- まず開きます [H1]\n- 次に押します [H2]",
  ];
  const out = placeHelpFigures(texts, [cite(1, figs(3)), cite(2, figs(5)), cite(3, [{ url: FIG2, alt: "x" }])], 4);
  expect(out[0]).toBe(texts[0]);
  expect(out[1]).toBe(texts[1]);
  const added = out[2].split("\n").filter((l) => l.includes("![")).length;
  expect(added).toBe(4);
  expect(out[2]).not.toContain(FIG2);
  expect(out[2].split("\n")[1]).toMatch(/^ {2}!\[0\]/);
});

test("http や記法を壊す URL は差し込まない", () => {
  const text = "1. 開きます [H1]";
  const [out] = placeHelpFigures([text], [cite(1, [{ url: "http://x/a.png", alt: "a" }, { url: "https://x/a b).png", alt: "b" }])]);
  expect(out).toBe(text);
});

test("リンクやコードに書いた図の URL では足すのをやめない", () => {
  const text = `1. 開きます [H1]。[図を見る](${FIG1}) \`${FIG2}\``;
  const [out] = placeHelpFigures([text], [cite(1, [{ url: FIG1, alt: "a" }, { url: FIG2, alt: "b" }])]);
  expect(out.split("\n")).toEqual([text, `   ![a](${FIG1})`, `   ![b](${FIG2})`]);
});

test("インラインコードの中の番号は差し込む位置にしない", () => {
  const text = "1. `[H1]` という表記です。\n2. 保存します [H1]。";
  const [out] = placeHelpFigures([text], [cite(1, [{ url: FIG1, alt: "a" }])]);
  expect(out.split("\n")).toEqual(["1. `[H1]` という表記です。", "2. 保存します [H1]。", `   ![a](${FIG1})`]);
});

test("項目の継続行で引いたときは、その項目の下に字下げして足す", () => {
  const text = "1. 個人設定を開きます。\n   パスワードを選びます [H1]。\n   続けて入力します。\n2. 保存します。\n\n最後に確認します [H2]。";
  const [out] = placeHelpFigures([text], [cite(1, [{ url: FIG1, alt: "a" }]), cite(2, [{ url: FIG2, alt: "b" }])]);
  expect(out.split("\n")).toEqual([
    "1. 個人設定を開きます。",
    "   パスワードを選びます [H1]。",
    "   続けて入力します。",
    `   ![a](${FIG1})`,
    "2. 保存します。",
    "",
    "最後に確認します [H2]。",
    "",
    `![b](${FIG2})`,
  ]);
});
