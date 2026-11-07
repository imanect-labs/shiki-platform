// チャットの ```ui（Yui）の説明を `@imanect/yui/guide` から生成する（手書きしない。手書きの説明は
// 5 種類のまま追随が止まり、円グラフを「描けない」と答えていた）。
//
//   cd web && pnpm gen:yui
//
// 出力 crates/chat/src/worker/yui_reference.json は commit する（サーバーは include_str! で読む）。
// web/package.json の `@imanect/yui` を上げたら作り直す。版のずれは Rust のテスト
// （crates/chat の yui_reference_matches_web_dependency）と CI の差分検査が落とす。
import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { KINDS } from "@imanect/yui";
import { GUIDE_RULES, guideIndex, kindGuide } from "@imanect/yui/guide";

const OUT = fileURLToPath(new URL("../../crates/chat/src/worker/yui_reference.json", import.meta.url));
const pkg = JSON.parse(readFileSync(fileURLToPath(import.meta.resolve("@imanect/yui/package.json")), "utf8"));

// 質問（ask）・計画（plan）のカードは実行前フェーズの門（crates/chat/src/worker/gate.rs）が
// 書き方を添えて指示するときだけ使うので、早見には載せない（書き方は ui_reference で読める）。
const INDEX_KINDS = KINDS.filter((k) => k !== "ask" && k !== "plan");

const reference = {
  yui_version: pkg.version,
  rules: GUIDE_RULES,
  index: guideIndex({ kinds: INDEX_KINDS }),
  kinds: Object.fromEntries(KINDS.map((k) => [k, kindGuide(k)])),
};
writeFileSync(OUT, `${JSON.stringify(reference, null, 2)}\n`);
console.log(`[gen:yui] @imanect/yui ${pkg.version} → ${OUT}`);
