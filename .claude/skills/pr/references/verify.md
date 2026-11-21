# 動作確認（起動・スクリーンショット・E2E）

UI/挙動が変わる変更では、**実装して終わりにしない**。起動し、目で見て、直して、また見る。

## 1. 起動

```bash
.claude/skills/pr/scripts/dev-up.sh                  # 既定（native）
.claude/skills/pr/scripts/dev-up.sh --rag            # 検索/RAG も使う（qdrant + ingestion-worker）
.claude/skills/pr/scripts/dev-up.sh --rag-remote     # RAG を使い、worker と S3 は外部（SHIKI_DEV_RAG_WORKER_URL 等・dev-up.sh 冒頭参照）
.claude/skills/pr/scripts/dev-up.sh --sandbox        # コード実行を使う（sandbox-orchestrator）
.claude/skills/pr/scripts/dev-up.sh --compose --office   # Word/Excel を使う（collabora・compose 必須）
.claude/skills/pr/scripts/dev-up.sh --compose        # shiki-server も compose（CI と同一構成）
.claude/skills/pr/scripts/dev-up.sh --status         # 生存確認だけ
.claude/skills/pr/scripts/dev-up.sh --down           # 停止
```

既定（native）は compose 依存だけ起動し、shiki-server はローカルビルドを使う。**Rust を変更した時の反復が速い**（docker イメージの再ビルドが不要）。ただし**初回は workspace の cold build で十数分かかる**。
`--compose` は CI の web-e2e と同一構成だが、Rust を変えるたびにイメージ再ビルドが要る。

RAG / sandbox / office は既定で**オフ**。該当機能を触る変更ではフラグで有効化する（オフのまま検証して「動かない」と誤診しないこと）。

やっていること（手で組む場合の要点）:

1. compose 依存サービスを起動する（postgres / keycloak / openfga / redis / rustfs ＋ フラグに応じて qdrant / ingestion-worker / sandbox-orchestrator / collabora）。
2. `shiki-server` を `:8080` で起動する。必須 env:
   - `SHIKI__AUTH__REDIRECT_URI=http://localhost:3000/auth/callback` — **`:8080` にすると callback 後に backend ルートへ 303 して落ちる。** CI と同じ値にする。
   - `SHIKI__AUTH__POST_LOGOUT_REDIRECT_URI=http://localhost:3000/`
   - `SHIKI_DEV_SEED=true` — シードが無いとログイン後に何も無い。
3. `web` を `:3000` で `pnpm dev` する。

生存確認: `curl -fsS http://localhost:8080/healthz` と `http://localhost:3000`。
ログインは compose のテストユーザー `alice` / `password`（`deploy/keycloak/shiki-realm.json`）。

### 起動まわりの罠

- **`migration N was previously applied but has been modified` で起動できない。** compose の `shiki` DB に別ブランチの migration が当たっている。dev DB は使い捨てなので作り直してよい:
  `dev-up.sh --reset-db`（`SHIKI_DEV_SEED=true` がシードを再投入する）。
  ブランチを行き来する開発では日常的に踏むので、起動できない時はまずこれを疑う。
- **新しい worktree には `web/node_modules` も `web/src/generated/` も無い**（生成物は `.gitignore` 済み）。
  どちらが欠けても `next dev` は動かない（`Command "next" not found` / 型解決不能）。
  `dev-up.sh` は欠けていれば `pnpm install` → `pnpm gen:api` を自動で走らせる（初回は数分）。
- **shiki-server を二重に起動しない。** 2 個目は `:8080` の bind に失敗しても**終了せず**、
  chat 生成ワーカーとワークフローワーカーだけが二重に走って同じジョブキューを取り合う。
  しかも古い方が `/healthz` に応答するため「起動成功」に見える。`dev-up.sh` は起動前に
  既存プロセスを停止するが、手で起動する場合は自分で確認すること
  （`pgrep -af 'target/debug/shiki-server'`）。

- **稼働中の `next dev`（:3000）と同じ worktree で `pnpm build` / `next build` を回すと `.next` を上書きして dev サーバが壊れる。** 以後ログイン callback が `waitForURL` タイムアウトし、一見テストコードの不具合に見えるが環境要因。本番ビルド検証は別ディレクトリか dev 停止中に行う。
  復旧: dev を止める → `rm -rf web/.next` → `pnpm dev`。
- **古い next-server が `:3000` に残っていると再ビルド後のチャンクと不整合で 400 / ChunkLoadError になる。** Playwright は `reuseExistingServer` で拾ってしまうので気づきにくい。
- **プロセス停止に `pkill -f 'next'` を使わない。自分のシェルの引数列にもマッチして自爆する**（exit 144 の連鎖）。ポート指定で殺す: `fuser -k 3000/tcp`。パターンで殺すなら `pkill -f "[n]ext-server"`。
- ハーネスに殺されない長時間プロセスは、スクリプトファイルに env を閉じ込めて
  `setsid nohup <script> > log 2>&1 < /dev/null & disown`。

## 2. スクリーンショット / 動画

`web/e2e/` に使い捨ての spec を書き、既存 dev サーバに対して回すのが最短。`helpers.ts` の `loginAs` / `loginViaKeycloak` / `uniqueName` を使う。

```ts
// web/e2e/_shot.spec.ts（使い捨て。確認後に必ず削除する。コミットしない）
import { test } from "@playwright/test";
import { loginViaKeycloak } from "./helpers";

test.use({ deviceScaleFactor: 2 });

for (const theme of ["light", "dark"] as const) {
  test(`対象画面 ${theme}`, async ({ page }) => {
    await page.emulateMedia({ colorScheme: theme });
    await loginViaKeycloak(page);
    await page.goto("/<対象パス>");
    await page.waitForLoadState("networkidle");
    // 出力先は test-results/ にする（web/.gitignore 済み。他の場所だと
    // 生成画像を誤ってコミットする）。
    await page.screenshot({ path: `test-results/target-${theme}.png`, fullPage: true });
  });
}
```

```bash
cd web && E2E_BASE_URL=http://localhost:3000 pnpm exec playwright test e2e/_shot.spec.ts
```

撮った画像は Read ツールで開いて確認する（`web/test-results/*.png`）。確認が終わったら `_shot.spec.ts` を消す。

確認する軸:

- **ライト / ダーク両テーマ**（`emulateMedia({ colorScheme })`）。
- **`deviceScaleFactor: 2`** — 等倍だと線の歪みや余白のズレが見えない。
- **狭幅**（`page.setViewportSize({ width: 768, ... })`）とパネル開閉状態。
- 動きのある UI（画面遷移・共同編集・ドラッグ・ストリーミング）は `test.use({ video: "on" })` で録画し、実際に再生して見る。

撮ったら**必ず自分の目で見る**。崩れ・違和感を直して再撮影し、納得いくまで反復する。

`fullPage: true` はライブラリによっては壊れる（遅延レイアウトするコンポーネントは可視領域外が描画されない）。その場合はビューポートを大きくして `fullPage: false` で撮る。

### デザイン言語の確認軸

genui / 新規コンポーネントは既存画面（トップ・ドライブ・ワークフロー）の設計言語に厳密に合わせる:

- 選択状態は**塗り**（`bg-accent`）。`border-primary` の黒枠は使わない。
- 区切りは `shiki-dash`。
- 枠は `border-border/60` ＋ `bg-card/40`。

### 撮った画像を独立に評価させる

自分が作った UI は「作ったとおり」に見える。**意図しなかったものが写り込んでいても気づかない**
（フォールバック表示・空状態・崩れたグリッド・切れたラベル）。

自分の目で見た**後**に、意図を知らないサブエージェントにも同じ画像を見せる
（`subagent_type: "general-purpose"`）。**撮影自体は委譲しない**（往復コストだけ増える）。

```
次の画像を見て、UI として破綻している箇所・不自然な箇所を挙げよ。

画像:
- <ROOT>/web/test-results/<name>-light.png（ライトテーマ）
- <ROOT>/web/test-results/<name>-dark.png（ダークテーマ）
- <ROOT>/web/test-results/<name>-narrow.png（幅 768px）

この製品の設計言語（既存画面と揃っているべきもの）:
- 選択状態は塗り（bg-accent）。border-primary の黒枠は使わない。
- 区切りは shiki-dash。
- 枠は border-border/60 ＋ bg-card/40。
比較対象として <ROOT>/web/src/components/ 配下の既存画面のコードを読んでよい。

見るもの:
- 余白・整列のズレ、はみ出し、重なり、切れたテキスト、スクロールバーの二重出現。
- ライト / ダークでのコントラスト不足、片方のテーマだけ壊れている箇所。
- 狭幅での折り返し崩れ、押せない大きさのヒット領域。
- 空状態・ローディング・エラー表示が「未実装の素の状態」に見えていないか。
- 明らかに機能していない要素（プレースホルダ、404 のアイコン、フォールバック文言）。

出力は次の形式のリストのみ。前置き・褒め言葉は書かない。問題が無ければ「所見なし」とだけ返す。

- where: <画像名と、画像内の位置（左上/中央のカード/ヘッダ右 など）>
  issue: <何が破綻しているか 1 文で>
  severity: high | medium | low

制約:
- 「〜だともっと良い」という好みの提案は出さない。破綻だけを出す。
- 画像から読み取れないことを推測で書かない。
```

**この画面が何をするものかをプロンプトに書かない。** 書くと「その説明に合っているか」しか見なくなり、
説明していない破綻（フォールバックに落ちた画面など）を見逃す。

## 3. 該当 E2E spec の実行（回帰確認）

既存 dev サーバを再利用して、変更領域に対応する spec だけ回す:

```bash
cd web && E2E_BASE_URL=http://localhost:3000 pnpm exec playwright test e2e/<対象>.spec.ts
```

`E2E_BASE_URL` を渡すと `webServer` を立てず既存サーバを使う（`playwright.config.ts`）。CI と違い直列（`workers: 1`）なので、複数 spec を指定すると時間がかかる。**変更領域に絞る。**

### 該当 spec の見つけ方

**spec 名の一覧をここに書き写さない**（追加・改名で即座に嘘になる）。毎回リポジトリから導出する。

**この探索は委譲してよい**（`subagent_type: "Explore"`）。50 本超の spec を本体で読むと予算を食うが、
返ってほしいのは「実行すべき spec のパス」だけ。下の手順 1〜4 をそのままプロンプトに貼り、
「変更されたパス一覧」と「実行すべき spec のパスのみを返せ。無ければ『該当なし』」を添える。
**実行は委譲しない**（結果の判定は自分で見る）。

```bash
# 0. base を導出する（remote-tracking ref。ローカル main は古いことが多い）
BASE=$(git symbolic-ref --short refs/remotes/origin/HEAD 2>/dev/null); BASE=${BASE:-origin/main}

# 1. 変更した web の領域を出す
git diff --name-only "$BASE"...HEAD -- web/src \
  | sed -E 's|web/src/components/([^/]+)/.*|\1|;t;d' | sort -u

# 2. 実在する spec を一覧する（これが正）
ls web/e2e/*.spec.ts

# 3. 領域名で当たりを付ける
ls web/e2e/ | grep -iE '<領域名>'

# 4. 名前で当たらなければ、変更した画面の文言・aria-label・data-testid・route で逆引きする
grep -rl '<変更した画面に出る文言>' web/e2e/
```

4 が効くのは、spec が `getByRole(..., { name: "新規作成" })` のように**画面の文言で要素を引いている**ため。逆向き（spec の文言から実装を探す）も同じ手で通る。

**`playwright test` に存在しないパスを渡すと「0 tests」で終わり、成功と見分けがつかない。** 実行後に必ず実行件数を確認し、0 件なら spec 名が間違っている（`ls` で実在を確かめる）。

ファイル名のパターンで分かること:

- `*.manual.spec.ts` — 実 LLM 等を要する手動実行用。CI では回らない。必要な時だけ意図的に実行する。
- `*real-llm*` — 実プロバイダが必要。判定は**「成果物が出たか」**で行う（ステップ数やトークン数ではなく）。
- `*video*` — 動画録画を伴う確認用。
- `visual*` — エディタ横断の見た目確認。UI に触ったら候補に入れる。

## 4. compose smoke（認証・起動経路を変えた時）

```bash
bash deploy/compose/smoke-bff.sh     # /healthz → /auth/login → Keycloak → callback → /me（+ Cookie 無しで 401）
```

## 4.5 preview（共有のプレビュー環境）で確かめる

ローカルで起動できないとき（クラウド環境で compose を立てられない等）や、**デプロイ済みの版**の挙動を確かめたいとき（不具合の再現など）に使う。
preview に載っているのはデプロイした版で、**作業中のブランチではない。** 未マージの変更はここでは確かめられない。

入口は 2 枚ある。

| 壁 | 通り方 |
| --- | --- |
| Cloudflare Access（`shiki-preview.imanect.app` と `shiki-preview-auth.imanect.app`） | サービストークンのヘッダ 2 つを**全リクエスト**に付ける |
| shiki のログイン（Keycloak） | テストユーザーでログインフォームを通す |

資格情報は環境変数で受け取る。**値をコード・ログ・PR・コメントに書かない。** 無ければ human に頼む（推測で探さない）。

| 変数 | 中身 |
| --- | --- |
| `CF_ACCESS_CLIENT_ID` / `CF_ACCESS_CLIENT_SECRET` | エージェント用のサービストークン |
| `SHIKI_PREVIEW_USER` / `SHIKI_PREVIEW_PASS` | テストユーザー。別テナントが要るときは、別のテストユーザーを human から受け取る |

疎通確認:

```bash
H=(-H "CF-Access-Client-Id: $CF_ACCESS_CLIENT_ID" -H "CF-Access-Client-Secret: $CF_ACCESS_CLIENT_SECRET")
curl -s -o /dev/null -w '%{http_code} %{redirect_url}\n' "${H[@]}" https://shiki-preview.imanect.app/login
```

`200`、または shiki 側へのリダイレクト（`302` / `307` で、2 列目が `shiki-preview` のホスト）なら通れている。2 列目が Cloudflare Access のログイン画面なら、ヘッダが付いていないか、トークンが違う。

Playwright では、**トークンを preview の 2 ホストへのリクエストにだけ付ける。** コンテキストの `extraHTTPHeaders` は使わない（外部のリンク・画像・スクリプトなど、**全ホスト**へのリクエストにトークンが付いて漏れる）。
`context.route` でホストを見て付ける。ログインの途中で `shiki-preview-auth` へリダイレクトするので、ページ単位ではなくコンテキスト単位で設定する。

```ts
const PREVIEW_HOSTS = new Set(["shiki-preview.imanect.app", "shiki-preview-auth.imanect.app"]);
const ACCESS_HEADERS = {
  "CF-Access-Client-Id": process.env.CF_ACCESS_CLIENT_ID!,
  "CF-Access-Client-Secret": process.env.CF_ACCESS_CLIENT_SECRET!,
};

const context = await browser.newContext({ baseURL: "https://shiki-preview.imanect.app", locale: "ja-JP" });
await context.route("**/*", (route) => {
  const req = route.request();
  if (PREVIEW_HOSTS.has(new URL(req.url()).hostname)) {
    return route.continue({ headers: { ...req.headers(), ...ACCESS_HEADERS } });
  }
  return route.continue();
});
// 確認: preview 以外のホストへのリクエストにトークンが付いていたら止める
context.on("request", async (req) => {
  if (PREVIEW_HOSTS.has(new URL(req.url()).hostname)) return;
  const h = await req.allHeaders();
  if (h["cf-access-client-id"] || h["cf-access-client-secret"]) {
    throw new Error(`Access のトークンが preview 以外へ送られた: ${new URL(req.url()).hostname}`);
  }
});

const page = await context.newPage();
await page.goto("/login");
await page.waitForURL(/shiki-preview-auth\.imanect\.app\/realms\/shiki\//);
await page.locator("#username").fill(process.env.SHIKI_PREVIEW_USER!);
await page.locator("#password").fill(process.env.SHIKI_PREVIEW_PASS!);
await page.locator("#kc-login").click();
await page.waitForURL((u) => u.hostname === "shiki-preview.imanect.app" && !u.pathname.startsWith("/login"));
```

`route.continue` で付けたヘッダは、そのリクエストが起こしたリダイレクトにも引き継がれる。preview の 2 ホストから外部へリダイレクトする操作（外部サービスへのログイン・連携など）は、この設定のまま行わない。

**preview は共有の環境。** 他の人も同じテストユーザーで、実際の業務に近い使い方をしている。

- 既存のファイル・テーブル・設定を消したり変えたりしない。組織の設定・AI 設定は触らない。
- 作るものは名前を `agent-` で始め、確認が終わったら消す。
- 渡されたテストユーザー以外でログインしない。
- 画面や応答に、テスト用でないデータ（利用者の文書・会話）が見えたら、スクリーンショットや PR に載せない。

## 5. PR への反映

最終スクリーンショットと、実行した E2E spec ＋ 結果を PR 本文の「## 検証」に書く。
実装が無い（設計/docs のみの）変更は「N/A（docs のみ）」と明記する。
