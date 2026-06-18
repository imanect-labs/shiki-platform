# 認証トークンのブラウザ保管方式（再検討メモ / ADR）

- **ステータス**: 確定（2026-06-18 human 合意済み。§7 参照。実装は未着手）
- **日付**: 2026-06-18
- **関連**: design.md §4.1 / design-caveats.md PIT-26〜30 / docs/auth/skillex-identity.md / roadmap phase-9 Task 9.6
- **正本との関係**: design.md が確定したら本書は背景記録（ADR）として残す。設計を変えるのは design.md。

> 本書は「フロントは OIDC JWT を取得し `Authorization` ヘッダで送信」（design.md §4.1）という現行方式を、
> **ブラウザにセッションID（不透明 Cookie）を置く方式**と比較して再検討した記録。
> 二択は **(A) BFF + オパークセッション Cookie（推奨）** vs **(B) 現行 localStorage JWT 維持**、
> および軽量移行の **(中間) JWT を httpOnly Cookie 化**。

---

## 1. 背景：現行方式と当初の採用根拠

現行（Phase 0 実装）:

- ブラウザ: `oidc-client-ts` の `UserManager` が **access + refresh トークンを `localStorage` に保管**（`web/src/lib/auth.ts`）、`automaticSilentRenew: true` で自動更新。
- API 呼び出し: `authedFetch()` が `Authorization: Bearer <jwt>` を付与（`web/src/lib/api.ts`）。
- サーバ: `require_auth` ミドルウェアが JWKS で RS256 署名・`aud`/`iss`/`exp` を検証（`crates/api/src/middleware/auth.rs`）。

当初の JWT 採用根拠（design.md §4.1 / PIT 群より）と、その再評価:

| 当初の根拠 | 再評価 |
|---|---|
| ステートレス検証（エアギャップ NFR-2） | エアギャップ要件は「**外部依存ゼロ**」であり「**状態ゼロ**」ではない。データプレーン側にローカルなセッションストア（Redis）を持てば成立。ブラウザが JWT を持つ必然性にはならない |
| 顧客ごと隔離セル / マルチテナント | セッションをテナント間で同期する要件は無い。**共用のセッションストアを `tenant_id` でスコープ**すれば隔離は成立する（顧客ごと専用インスタンスは不要・§7.2 参照） |
| 外部IdP フェデレーション | Keycloak が OIDC を喋るのは不変。BFF がトークンを**サーバ側で保持**するだけでフェデレーションは無影響 |
| サービス間で identity を運ぶ | ✅ JWT が正しい。ただし**内部の話**で、ブラウザ保管形式とは独立 |

**結論**: JWT の強みは全て「バックエンド／内部・サービス間」で活きる。**「ブラウザが JWT を localStorage に持つ」という選択を正当化しているものは一つもない**。

---

## 2. 現行方式の問題点（PIT-30）

1. **XSS でトークン総取り**: `localStorage` は任意の JS から読める。1回の XSS で access + refresh の両方が流出し、**長期のアカウント乗っ取り**になる。`draft-ietf-oauth-browser-based-apps` は新規ブラウザアプリでの localStorage 保管を非推奨とし BFF を推奨している。
2. **失効が即時に効かない**: JWT は `exp` まで有効。design-caveats PIT-27（剥奪が stale）、phase-1 Task 1.6「共有解除で即時アクセス不可」という**製品要件と相性が悪い**。
3. **副次**: design.md が書く「SSE は fetch-stream でヘッダ付与」という回りくどさは、Cookie 方式なら Cookie が自動添付され `EventSource` がそのまま使えるため不要になる。

---

## 3. 選択肢の比較

| 観点 | (A) BFF + オパーク Cookie【推奨】 | (中間) JWT を httpOnly Cookie | (B) 現行 localStorage JWT |
|---|---|---|---|
| XSS でのトークン流出 | 🟢 不可（JS から読めない・トークンはサーバ側） | 🟢 不可（JS から読めない） | 🔴 容易（access+refresh 流出） |
| 失効の即時性（Task 1.6） | 🟢 サーバセッション削除で即時 | 🔴 exp まで有効（弱点残存） | 🔴 exp まで有効 |
| CSRF | 🟡 SameSite + CSRF トークン要 | 🟡 同左 | 🟢 ヘッダ方式なので原理的に低い |
| エアギャップ / 隔離セル | 🟢 データプレーン側のセッションストアで成立（共用Redis＋tenant_idスコープ・§7.2） | 🟢 影響なし | 🟢 影響なし |
| 内部/サービス間のステートレス性 | 🟢 維持（BFF がJWT転送／境界のみステートフル） | 🟢 維持 | 🟢 維持 |
| 追加インフラ | 🔴 セッションストア（**Redis** に決定。§7） | 🟢 不要 | 🟢 不要 |
| SSE | 🟢 Cookie 自動添付で簡潔化 | 🟢 同左 | 🟡 fetch-stream でヘッダ注入 |
| 移行コスト | 🔴 大（BFF エンドポイント＋セッション層 新設） | 🟡 中 | 🟢 ゼロ |
| エンタープライズ監査での印象 | 🟢 推奨構成 | 🟡 可 | 🔴 ほぼ確実に指摘 |

**推奨: (A)**。理由は (1) XSS 耐性、(2) **失効即時化が製品要件 Task 1.6 / PIT-27 を素直に満たす**こと。ステートフル化はブラウザ⇄api 境界のみで、内部はJWTのままなので不変条件（単一チョークポイント・アンビエント権限禁止）は維持できる。

軽量に進めるなら **(中間)** を先行（XSS だけ先に潰す）→ 失効要件のために最終的に (A)。

---

## 4. 推奨構成 (A) の概形

- **ブラウザ ⇄ api**: `httpOnly` + `Secure` + `SameSite=Lax/Strict` の**不透明セッション Cookie** のみ。トークンは一切ブラウザに置かない。
- **api（BFF 役）**: OIDC Authorization Code + PKCE の **code 受け／token 交換をサーバ側**で実施し、OIDC token をセッションストアに保管。リクエストごとに Cookie → セッション → `Principal` を復元。
- **api ⇄ 内部/サービス間（skillex 等）**: 従来どおり JWT / token-exchange。**ここは無変更**。
- **失効**: サーバ側セッション削除で即時。OpenFGA 剥奪（PIT-11 の HIGHER_CONSISTENCY）と組み合わせる。
- **CSRF**: `SameSite` ＋ CSRF トークン（double-submit）。

### Phase 9 app-gateway(BFF) との区別（重要）

phase-9 Task 9.6 の「BFF（`crates/app-gateway`）」は**ミニアプリ用の公開APIゲートウェイ**であり、本書の**メインWebアプリ認証 BFF（`crates/api` の auth セッション境界）とは別物**。両者を混同しないこと。本書の変更は `crates/api` 側。

---

## 5. 影響範囲（コード調査結果）

### 5.1 再利用できる（触らなくて済む）

| 対象 | 理由 |
|---|---|
| `crates/api/src/middleware/claims.rs`（`principal_from_claims`） | claims → `Principal` 生成は共通。入力元が JWT → セッション内 claims に変わるだけ |
| `crates/api/src/extract/principal.rs` / `extract/auth_context.rs` | extension から `Principal`/`AuthContext` を取る extractor は不変 |
| `crates/api/src/routes/me.rs` 等 既存エンドポイント | `AuthContextExt` の入力元が変わるだけ |
| OpenFGA 認可チェック | `Principal` さえ得られれば不変 |
| JWT 署名/`aud`/`iss`/`exp` 検証ロジック | token 交換後の ID/Access token 検証で再利用 |

### 5.2 変更が必要 / 新規

| ファイル | 種別 | 内容 |
|---|---|---|
| `web/src/lib/auth.ts` | 置換 | localStorage 保管・silent renew を撤去。code 生成/PKCE のみ、または全面サーバ移管 |
| `web/src/lib/api.ts` | 変更 | Bearer 付与撤去。`fetch(..., { credentials: 'include' })` |
| `web/src/app/callback/page.tsx` | 置換 | code を BFF エンドポイントへ POST（サーバが token 交換） |
| `web/src/app/page.tsx` | 変更 | logout を BFF `/auth/logout` 経由に |
| `web/.env.example` / `web/package.json` | 整理 | `NEXT_PUBLIC_OIDC_*` 削減、`oidc-client-ts` 依存縮小 |
| `crates/api/src/middleware/auth.rs` | 置換 | Bearer 検証 → セッション Cookie 検証 |
| `crates/api/src/middleware/session.rs` | 新規 | Cookie → セッション → `Principal` ミドルウェア |
| `crates/api/src/routes/auth/{login,callback,logout}.rs` | 新規 | OIDC code 受け／token 交換／Cookie 発行・破棄 |
| `crates/api/src/config.rs` / `state.rs` | 拡張 | token endpoint、セッション TTL、Cookie 属性、セッションストアクライアント |
| `crates/api/src/openapi.rs` | 変更 | security scheme を Bearer → Cookie |
| `crates/api/tests/http.rs` | 変更 | 401 テストを Cookie 無しベースに |
| `deploy/compose/docker-compose.yml` | 追加 | **Redis** サービス（セッションストア） |
| `docs/design.md` §4.1 | 更新 | 確定後に方式を反映（SSE 記述含む） |

### 5.3 不変条件への影響

| 不変条件 | 影響 |
|---|---|
| AuthN=Keycloak / 認可=OpenFGA / 単一チョークポイント | 🟢 維持（`Principal` 取得元が変わるだけ） |
| 内部/サービス間のステートレス JWT | 🟢 維持（ステートフルはブラウザ⇄api 境界のみ） |
| CORS | 🟡 Cookie credential 対応の厳密化（`Allow-Credentials: true`、オリジン固定） |

---

## 6. skillex 連携への影響（軽め調査）

- skillex は **server-to-server（client_credentials）**でトークンを取得し、`aud=shiki-llm` で shiki の DLC/LLM を叩く設計（`docs/auth/skillex-identity.md`、`deploy/keycloak/shiki-realm.json` の `skillex` client、CI `ci.yml` で検証済み）。**ユーザーのブラウザを経由しない**。
- したがって **BFF 化の skillex への直接影響はほぼ無い**。skillex のトークン取得経路・`aud` 束縛・confused-deputy 防御（PIT-27）は不変。
- 唯一の注意: ブラウザが `aud` を検証する余地は元々無いので、`aud`/`scope` 厳密検証の責務は引き続き **api／サービス側**にある（PIT-27 のまま）。BFF 化でこの結論は変わらない。
- skillex web app 自身の OIDC ログイン（roadmap SK.4）は共有 realm へのログインであり、shiki 側 BFF 化とは独立。

---

## 7. 確定事項（2026-06-18 human 合意済み）

| # | 論点 | 決定 | 補足 |
|---|---|---|---|
| 1 | 移行方針 | **(A) フル BFF を一括で** | 中間案を挟まず、Phase 0 のうちに BFF + オパーク Cookie へ移行。Task 1.6 / PIT-27 の即時失効を一発で満たす |
| 2 | セッションストア | **Redis（プール型・全テナント共用1クラスタ/HA）** | TTL/失効・高頻度アクセスに最適の業界標準。**全テナントで共用する単一の Redis** にし、セッションキーを **`tenant_id` でスコープ**して論理分離。**顧客ごとの専用インスタンスは作らない**（顧客数に比例して運用が破綻するため。専用環境を契約した大企業向けの有償オプションでのみ例外）。compose に Redis を追加。**オンプレ縮退（PIT-29）**として「Redis 依存」を縮退仕様に明記。詳細は §7.2 |
| 3 | SameSite / CSRF | **SameSite=Lax + double-submit CSRF トークン** ＋ **web/api を同一オリジン配信** | 下記 7.1 のトポロジ確認結果に基づく |
| 4 | design.md §4.1 | **更新済み** | §4.1 を BFF 方式に書き換え、§2 図の `OIDC JWT` → `セッションCookie` に修正 |

### 7.1 web/api のトポロジ確認（論点3の根拠）

現状（`web/.env.example` / `deploy/compose/docker-compose.yml`）:

- dev では web(Next.js, :3000) と api(shiki-server, :8080) は**別オリジン・同一サイト**（どちらも `localhost`、ポート違い。ポートは Cookie の "site" に含まれないため SameSite=Lax/Strict は越ポートで送出される）。
- 本番用のリバースプロキシは compose に**無く**、web は compose 未収録（`pnpm dev` 別建て）。Cookie は first-party 化されていない。

**決定**:
- **同一オリジン配信を前提化**する（リバースプロキシ or Next rewrites で `/api/*` → shiki-server）。セッション Cookie を first-party にし、CORS+credential の複雑さを避ける。
- **SameSite=Lax** を採用。OIDC の **state/PKCE 相関 Cookie** は Keycloak からの**トップレベル GET ナビゲーション（cross-site）で callback に戻る際に送出される必要があるため Lax 必須**（Strict だと callback で相関 Cookie が届かない）。セッション Cookie 本体も揃えて Lax。
- 状態変更リクエストは **double-submit CSRF トークン**で防御。
- dev で別ポート運用を続ける場合は、CORS `Allow-Credentials: true` ＋ オリジン固定、または Next dev rewrite で同一オリジン化する。

### 7.2 マルチテナント時の Redis トポロジと PIT-26 整合（論点2の根拠）

**結論**: セッション Redis は **プール型（全テナント共用の1クラスタ・HA）＋ `tenant_id` によるキースコープ**。顧客ごとの専用インスタンスは既定では作らない。

- **なぜ専用インスタンスにしないか**: 顧客数に比例してインスタンスが増え、プロビジョニング・監視・アップグレード・コストが破綻する（silo 型の弊害）。SaaS の定石どおり**プール型＋論理分離**を採る。専用インスタンスは「専用環境を契約した大企業向け」の有償オプションとしてのみ。
- **隔離の担保**: 全セッションキーを `tenant_id`（将来の継ぎ目, design §137）でスコープし、あるテナントが他テナントの `session_id` を引けないようにする。**ハードな物理隔離が必要なのは Redis ではなくデータプレーン本体（RAG 実体・ファイル・ReBAC）**側。
- **PIT-26 との整合**: PIT-26 が警告する爆心地は **共用 Keycloak（identity の正本）**。セッション Redis は (1) identity の正本ではない、(2) 失効可能、(3) 短命、(4) 中身のトークン自体も失効可能、であり、共用でも被害は Keycloak ほど大きくない。かつブラウザ側の blast radius 縮小は**オパーク化の時点でほぼ達成済み**（共用 Keycloak の本物トークンがブラウザに出ない）。よって共用プール型 Redis は許容できる。
- **物理構成の未決**: 「共用＋キースコープ／別 namespace／テナントごと別インスタンス」のどこまで物理分離するかは、データプレーンのセル化方針（未確定）に従う。本 ADR は既定を**共用プール＋`tenant_id` スコープ**とし、強い隔離が要件化したら昇格する。
- **オンプレ**: シングルテナントなので `tenant_id` スコープは実質単一。Redis は同梱（既に Postgres/Qdrant/OpenFGA/Keycloak/MinIO を同梱しており、追加は誤差）。

### 7.3 残タスク（実装フェーズで対応）

- §5.2 の新規/変更ファイル群の実装（BFF エンドポイント、session ミドルウェア、Redis 配線、Cookie/CSRF、web 改修）。
- セッションキーの **`tenant_id` スコープ**実装（共用プール型 Redis での論理分離・§7.2）。
- compose への Redis 追加と、オンプレ縮退仕様（PIT-29）への「Redis 依存」追記。
- 同一オリジン配信（リバースプロキシ / Next rewrites）の構成。
