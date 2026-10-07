# shiki web (Next.js BFF) のコンテナイメージ。
#
# **`pnpm gen:api` の生成物が web/src/generated/ に揃っている前提。**
# あれは Rust 定義から型を生成する処理で cargo を 4 回走らせる（CLAUDE.md の
# 「codegen が正」）。イメージの中で走らせると Rust のフルビルド（30 分級）を
# web のために二重に回すことになるので、**ビルド前に外で生成してから** ここへ
# 渡す（CI では既に Web ジョブが同じことをしている）。
#
# 生成物が無ければ**明示的に失敗させる**。黙って古い型でビルドすると、API と
# 食い違った BFF が出来上がって原因が分かりにくい。
#
# build context は web/。リポジトリルートではない。
FROM node:22-bookworm-slim AS builder
WORKDIR /app
RUN npm install -g pnpm@9

# 依存だけ先に入れてレイヤキャッシュを効かせる。
COPY package.json pnpm-lock.yaml ./
RUN pnpm install --frozen-lockfile

COPY . .

# 生成物の存在確認。`pnpm build` は prebuild で gen:api を呼ぶが、それには cargo が
# 要るのでここでは走らせない（`pnpm exec next build` で prebuild を飛ばす）。
RUN set -eu; \
    for f in src/generated/openapi.json src/generated/api.d.ts src/generated/authz-vocab.ts; do \
      [ -s "$f" ] || { echo "ERROR: $f が無い。先に \`pnpm gen:api\` を実行してからビルドすること。" >&2; exit 1; }; \
    done

# **BACKEND_ORIGIN はビルド時に routes-manifest.json へ焼き込まれる**
# （next.config.mjs の rewrites() 経由）。実行時に渡しても効かない。
# k8s ではクラスタ内 DNS を指す。
ARG BACKEND_ORIGIN=http://localhost:8080
ENV BACKEND_ORIGIN=${BACKEND_ORIGIN}
ENV NODE_ENV=production
# prebuild(gen:api) を飛ばして next build だけ走らせる。
RUN pnpm exec next build

# ----------------------------------------------------------------
FROM node:22-bookworm-slim AS runner
WORKDIR /app
ENV NODE_ENV=production
# **非 root で動かす。** PodSecurity "restricted" を満たすため。
# node イメージには uid 1000 の node ユーザが居る。
USER node

# standalone 出力は実際に使う node_modules だけを含む自己完結セット。
COPY --from=builder --chown=node:node /app/.next/standalone ./
COPY --from=builder --chown=node:node /app/.next/static ./.next/static
# web/public は存在しないので COPY しない（作ったら足すこと。無い状態で COPY すると
# ビルドが失敗する）。

EXPOSE 3000
ENV PORT=3000 HOSTNAME=0.0.0.0
# standalone は `next start` ではなく server.js を直接起動する。
CMD ["node", "server.js"]
