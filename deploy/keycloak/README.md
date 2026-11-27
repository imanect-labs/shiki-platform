# Keycloak realm（dev 専用）

`shiki-realm.json` は **開発・CI 専用の realm fixture** です。**本番環境にそのまま import してはいけません**（#91 L-5）。

## 本番使用が禁止な理由

この fixture は開発利便のために意図的に緩い設定を含みます:

- `"sslRequired": "none"` — 平文 HTTP を許可。本番は `"external"` 以上必須。
- 平文の client secret（`shiki-web-dev-secret` / `skillex-dev-secret` /
  `shiki-provisioner-dev-secret`）— そのまま使うと **provisioner service account 経由で
  `/admin/tenants` のテナント作成/削除まで奪取され得る**。
- redirect URI / `webOrigins` にワイルドカード（`/*` / `+`）— トークン奪取面が広い。
- ユーザーパスワードの直書き。

## 本番 realm の要件

- realm は別管理（IaC / Keycloak 管理 API）で構築し、この JSON を流用しない。
- client secret は Vault 等のシークレットストアから注入（リポジトリに置かない）。
- `sslRequired = external`、redirect URI / `webOrigins` を実ホストに限定。
- provisioner client は confidential・最小権限（realm 管理の必要スコープのみ）。
- `accessTokenLifespan` は失効遅延とのトレードオフで決める。
- **ユーザー属性 `tenant` / `roles` は管理者だけが閲覧・編集できる**（#809）。下記
  「User Profile」の設定を本番 realm にも必ず入れる。

## User Profile（`tenant` / `roles` は管理者のみ・#809）

shiki-server の `tenant_id` と `roles` は、ユーザー属性 `tenant` / `roles` を `shiki-web` の
protocol mapper で claim にしたものから来る（`crates/api/src/middleware/claims.rs`）。ユーザーが
アカウントコンソールや Account REST API（`/realms/<realm>/account`）で自分の `tenant` を書き換え
られる設定にすると、**次のログインで他テナントに入れる**（テナント越境）。`roles` も同様に
自己昇格になる。そのため realm の User Profile（`components` の
`org.keycloak.userprofile.UserProfileProvider` → `declarative-user-profile`）で次を必須とする:

- `tenant`（単一値）・`roles`（多値・`multivalued: true`）を**宣言する**。permissions は
  `view: ["admin"]` / `edit: ["admin"]`（ユーザー本人には見せず・書かせない）。
- **unmanaged attributes は無効**（`unmanagedAttributePolicy` を設定しない）。ユーザーが未宣言の
  属性を足せないようにする。`ENABLED`（ユーザーも編集可）は不可。
- 宣言を省いてはいけない。Keycloak 24 以降は unmanaged 無効のとき**未宣言属性を admin REST でも
  書けない・単体 GET で返さない**ため、テナント・プロビジョニング（`crates/api/src/keycloak_admin.rs`
  の作成時 `attributes.tenant`／`update_user_tenant`）が黙って効かなくなる（tenant claim の無い
  ユーザーは SaaS では fail-closed でログイン不可）。
- `groups` claim（org 解決）の元はグループ所属で、Account API からは変更できない（admin のみ）。
  ログインとセッションの更新（アクセストークンの寿命ごと）で、そこから決まる組織の `organization#member` を
  OpenFGA に書き、ほかの組織の member を外す（D-130・`crates/api/src/routes/auth/org_member.rs`）。グループから組織が
  決まらない人（既定の組織 `default` に落ちる人）には書かず、既定の組織の member（手で書いたもの）は残す（BUG-200）。
  - アプリのゲートウェイはこの member で組織を確かめる（D-130）。shiki の画面の中でアプリを使っている人は、画面の
    読み込み（承認の件数は 60 秒ごと）がセッションの更新を起こすので、shiki のアクセストークンの寿命の後に外れる。
  - **グループ・属性 `roles` から外した人の member と部署・ロール（`role#member`）は、ログインしなくても定期の照合で外れる**（D-136・D-140・
    `crates/api/src/membership_reconcile.rs`）。shiki の web にログインしないまま B1 のトークンだけを取る人（認可コードは
    利用者のブラウザーの URL に載るので、利用者が自分でトークンを取り換えられる）も、照合で member が外れた時点から
    ゲートウェイが 403 で断る。照合は外すだけで、付けるのはログインとセッションの更新だけ。
    - 照合は shiki-server が 60 秒ごとに、一度でもログインした人（ディレクトリ）を 500 人ずつ、provisioner の資格情報で
      Keycloak の管理 API（`GET /users/{id}/groups`、所属グループで決まらないロールの候補がある人は `GET /users/{id}` の
      属性 `roles` も）から読む。
      N 人を 1 周するのに N / 500 分かかる（1,000 人で 2 分、10,000 人で 20 分）。グループから外してから member が外れる
      までの遅れは、最長でこの 1 周（と次の回までの 60 秒）。設定は `membership_reconcile.interval_secs`（下限 10 秒）・
      `batch_size`（1〜1,000）・`enabled`。サーバーが何台あっても、照合は全体で `interval_secs` に 1 回だけ進める
      （advisory lock と、最後に照合を始めた時刻で決める）。Keycloak への読みはサーバーの台数で増えない（1 回に最大
      500〜1,000 回・全体で 1 分に 1 回ほど）。
    - 読む権限は provisioner の `manage-users`（`view-users` を含む）で足りる。この realm の provisioner はすでに持つので、
      **既存の realm に足すものは無い**。provisioner に `manage-users` を付けていない本番の realm では、realm-management の
      `view-users` を provisioner のサービスアカウントに付ける（管理コンソールの「Clients → shiki-provisioner →
      Service account roles → Assign role」で realm-management の `view-users`）。読めないと照合は誰も外さない（権限が
      無いときは、回ごとに 1 行「provisioner に利用者を読む権限がありません」が出る。届かないときは「Keycloak から所属を
      読めません」）。provisioner を設定しない構成では照合は動かない。
    - 外さないとき: Keycloak を読めない（接続できない・権限が無い）人、Keycloak にいない人（消した人はトークンを
      取れない）、OpenFGA を読めないとき。ログインの同期が記録していないロール（手で書いたタプル）には触れない。
      属性 `roles` から外したロールは、所属グループの部署にも無ければ外れる（ログインの同期と同じ決め方・D-140）。admin・audit_viewer にも触れない。既定の組織（`default`）の member も外さない
      （手で書いたものだけなので、外すのはログインの同期だけ。shiki-web に所属グループのマッパーが無い構成でも、
      照合が管理 API のグループで既定の組織を外して `/me` を 403 にすることはない・BUG-200）。
  - すぐに止めるには、これまでどおり Keycloak でその人を無効にする（Back-Channel Logout がセッションを切る）。
- **顧客 IdP / LDAP のマッパーで `tenant` / `roles` を顧客が制御できる値から取り込まない**。
  IdP の Attribute Importer（sync mode FORCE 等）や LDAP の user-attribute mapper はサーバ側で属性を
  直接書くため、上の `edit: ["admin"]` をすり抜ける（顧客 IdP が `tenant=<他社>` を出せば越境する）。
  テナントごとの IdP 連携で `tenant` を付けるなら、そのテナント固定値の hardcoded-attribute mapper にする。

dev fixture の `shiki-realm.json` は上記を設定済み。`deploy/compose/smoke-keycloak-user-profile.sh`
（CI の compose smoke でも実行）が、設定の確認・一般ユーザーが Account API で `tenant` / `roles` /
未宣言属性を書けないこと（再ログイン後の claim 不変）・管理者は admin REST で設定/検索/更新できる
ことを実機で確かめる。本番 realm でも、設定の確認（手順 1）は管理者資格情報で同じ API
（`GET /admin/realms/<realm>/users/profile`）を叩いて確かめること。

compose の Keycloak は `--import-realm`（既存 realm は `IGNORE_EXISTING`）のため、この設定より前に
realm を import 済みの dev DB には反映されない。Keycloak の DB（compose の postgres の `keycloak`
DB）を作り直すか、管理コンソールの「Realm settings → User profile」で同じ設定を入れること。

## 利用者の id（`sub`）とユーザーフェデレーション（D-205）

shiki-server はアクセストークンの `sub` を、そのまま OpenFGA の利用者の id（`user:<tenant>|<sub>`）にする。`sub` に
`:`・`#`・`|`・制御文字・空白を含む人は、ログイン・セッションの更新・アプリのゲートウェイで断る
（`crates/authz/src/ident.rs` の `validate_user_id`）。ログインはログイン画面に「このアカウントは使えません。管理者に
連絡してください。」を出し、ゲートウェイは 401 を返す。これらは OpenFGA の id の区切りの文字で、`:`・`#` を含む人は
組織の member にならず、`|` を含む人は member にはなっても定期の照合で外れないため。

- Keycloak の既定では `sub` は利用者の UUID なので、断られる人はいない。IdP 連携（Identity providers）で入る人も、
  Keycloak に取り込まれて UUID になる。
- **ユーザーフェデレーション（LDAP・Active Directory）では「Import users」を有効にする**（既定で有効）。管理コンソールの
  「User federation → 対象のプロバイダー → Synchronization settings → Import users」。無効にすると Keycloak は利用者を
  取り込まず、`sub` が `f:<プロバイダーの id>:<外部の id>` になって `:` で断られる。
  - 後から有効にすると、その人は次のログインで取り込まれ、新しい UUID の `sub` になる（shiki では新しい利用者になる）。
  - 有効かどうかは、管理 API の `GET /admin/realms/<realm>/components?type=org.keycloak.storage.UserStorageProvider`
    の `config.importEnabled` で確かめられる（`["false"]` のプロバイダーの人が断られる）。
- ログインで断った人の Keycloak の SSO セッションは、shiki-server が受け取った refresh token でサーバ側から終える
  （`POST /realms/<realm>/protocol/openid-connect/logout` に client の資格情報と `refresh_token` を送る・
  `crates/api/src/oidc.rs` の `end_sso_session`）。終えないと、ログイン画面を開き直しても Keycloak が同じ人を黙って
  通し、共有の端末でほかの人が入れない。ブラウザのログアウト（end-session へのフル遷移）を使わないのは、
  `id_token_hint` を出さないので Keycloak が確認の画面を挟み、戻り先も `post_logout_redirect_uri` に限られるため。
  ログイン画面は「別のアカウントでログイン」を出し、Keycloak のログイン画面へ送る。終えられなかったとき（Keycloak に
  届かない・3 秒を過ぎた）は warn のログに残す。そのときボタンを押すと、Keycloak が同じ人を黙って通して shiki に戻り、
  shiki-server がもう一度終えるのを試す（一時的な失敗なら、押し直せば Keycloak のログイン画面に出る）。Keycloak に
  届かないままなら、SSO の Cookie が切れるまで同じ画面に戻る。
- 定期の照合は、id が識別子に使えない人を飛ばし、回ごとに 1 行「id が識別子に使えない人を飛ばしました」を warn に
  出す。この決まりより前にログインした人の OpenFGA のタプルは自動では外れないので、手で外す。

## ミニアプリのクライアント（`app-<アプリの id>-b1` / `-b2`・BUG-172）

コードのミニアプリ（B1・B2）のクライアントは、この realm の JSON には無い。インストールのときに shiki-server が
provisioner の資格情報で登録する（`crates/app-gateway/src/oauth.rs` の `client_representation`）。

- B1（public・PKCE S256・アクセストークンは 5 分）の戻り先は `{web のオリジン}/apps/oauth/callback`（web シェルの
  ポップアップ・ホスト支援 PKCE）。web のオリジンは `gateway.web_origin`、無ければ `auth.redirect_uri` のオリジン。どちらもブラウザーの `location.origin` の形（既定のポートと末尾の `/` を書かない）にそろえて登録する。
- 既定のスコープは `basic` だけ（B2 は Keycloak が付ける `service_account` も）。トークンに `sub` を載せる `basic` は、
  Keycloak 25 以降の realm の既定のスコープ。**本番 realm に `basic` が無いと、アプリのトークンに `sub` が無く、
  ゲートウェイが 401 で断る。** 任意のスコープは能力スコープ（`data.read` など・サーバーが作る）。
- マッパーは aud（デプロイ設定 `gateway.audience`・既定 `shiki-gateway`。ゲートウェイが確かめる値と同じものを書く。
  値を変えると、次の起動の照合がすべての有効なインストールのクライアントを新しい値に揃え直す・BUG-206）と
  `tenant`（利用者の属性 `tenant`・上の User Profile の保護がそのまま効く）で、
  アクセストークンにだけ載せる。名前・メールアドレス・ロール・所属グループ（`groups`）は載せない（B1 では
  トークンがアプリのコードに渡る）。ゲートウェイは利用者の組織をトークンではなくインストールの組織で決め、その組織の
  `organization#member`（上のログインの同期が書く）を OpenFGA で確かめる。member でなければ 403（D-130）。
  所属するロールは、身元を読む能力（`identity.read`）を同意したアプリにだけゲートウェイが返す。7d の版が付けた
  `groups` のマッパーは、登録の形が変わるので起動時の照合で外れる。
- サーバーの関数（B2）を持つアプリの B1 には、同じアプリの B2 の client_id をアクセストークンの `aud` に足すマッパー
  （`shiki-function-audience`）も付ける（BUG-182）。利用者が起点の関数の呼び出しは、B1 で取った利用者のトークンを
  B2 が Keycloak 26.2 の標準のトークンの交換（RFC 8693）で交換する。実 Keycloak で確かめた決まりは次のとおり。
  - 交換を頼むクライアント（B2）が元のトークンの `aud` に入っていないと 403「Client is not within the token
    audience」で断る（自分に発行されたトークンを除く）。別のアプリの B2 は `aud` に入らないので交換できない。
  - `audience` はクライアントの id で、トークンの `aud` を絞るだけ（足さない）。ゲートウェイの aud（`gateway.audience`）は
    クライアントではなくマッパーが足す値なので、渡すと 400「Audience not found」になる。サーバーは `audience` を送らない。
  - `scope` は B2 の任意のスコープを足せる（元のトークンに無い能力スコープでも足せる）。サーバーは元のトークンの
    能力スコープだけを求め、返ったトークンの能力スコープがその内にあることを確かめる。
  - アンインストールで B1 を無効にすると、発行済みの B1 のトークンも交換できない。
- B2 の自動実行（event・cron）は、B2 の client_credentials のトークンでゲートウェイを呼ぶ。ゲートウェイは、`azp` が
  有効なインストールの B2 で、`sub` がその B2 のサービスアカウントの利用者（インストールが
  `app_installation.b2_service_sub` に書く）のトークンだけを、アプリの主体（`miniapp:<tenant>|<app_id>`）として扱う
  （BUG-183）。テナントと組織はトークンではなくインストールから決める。アプリの主体が使えるのはアプリのテーブルと
  通知の記録だけ。サービスアカウントの利用者の id は provisioner の `manage-clients` で読める（権限は足していない）。
- サーバーは起動時に、有効なインストールのクライアントを今の形に揃える（戻り先・スコープ・マッパー・有効）。
  **これらのクライアントに管理者が手で足したマッパーやスコープは、揃えるときに消える。** Keycloak から消えた B1
  （secret を持たない公開のクライアント）は作り直す（GAP-70）。**消えた B2 は作り直さない。** 漏れた secret を
  無効にするために消したのなら、作り直すと同じ secret のアプリの主体が戻るため（新しい secret で作り直すには、
  インストールした人が持つ secret を書き換える権限が要る）。B2 が消えたインストールは自動実行の主体を外し、
  インストールの画面が入れ直しが要ると知らせる。入れ直すと新しい secret の B2 を作り、保管し直す（アプリのテーブルは
  新しくなる）。揃えるのは登録の形か web のオリジンが変わった後の起動（と、作り直せなかったインストール）なので、
  **有効なインストールの B1 を Keycloak で消したり無効にしたりしても、次に揃えるときに戻る。アプリを止めるには shiki
  でアンインストールする。**
- **Keycloak で無効にした B2 は、照合でも有効に戻さない**（BUG-201・D-137）。インストールに印
  （`oauth_client_disabled`）を付けて自動実行の主体を外し、インストールの画面が「このアプリのサーバーで動く処理が
  止まっています」と知らせる。印のある行は起動のたびに試し直し、管理者が Keycloak で有効に戻していたら揃えて印を
  外す。**入れ直す（再インストール・版の更新）と、無効の B2 は secret を作り直してから有効に戻る**（止めた secret では
  アプリの主体のトークンを取れない。新しい secret は shiki が保管し直す。secret は shiki だけが持つので、作り直しても
  困る人はいない）。有効な B2 の入れ直しは secret を変えない。B2 の secret が漏れたときは、Keycloak でクライアントを
  無効にするか消して止め、shiki で入れ直す（どちらも新しい secret になる）。
- アンインストールはクライアントを無効にする（消さない）。再インストールで有効に戻り、今の形に揃う（B2 は secret を
  作り直す）。

## Back-Channel Logout（#91）

`shiki-web` client には OIDC Back-Channel Logout を設定済み（dev fixture）:

- `backchannel.logout.url` = `http://localhost:10067/auth/backchannel-logout`（BFF の受け口）。
- `backchannel.logout.session.required` = `true`（logout_token に `sid` を載せ、当該 SSO
  セッションのみ失効させる）。

これにより、管理者が Keycloak でユーザーを**無効化/削除**すると Keycloak が BFF へ
`logout_token` を POST し、`accessTokenLifespan`（dev は 1800s）を待たずにサーバ側セッションが
即時失効する。BFF は logout_token の署名・iss・aud（= client_id）・logout イベント宣言・
`nonce` 不在を検証してから失効する（`crates/api/src/routes/auth/backchannel_logout.rs`）。

**本番では** `backchannel.logout.url` を実ホスト（BFF の公開 URL）へ差し替えること。
BFF は Keycloak から到達可能である必要がある（同一ネットワーク or 到達可能な URL）。

## ログイン画面（Shiki テーマ）とブラウザフロー

ログイン画面は Keycloak のログインテーマ `themes/shiki/login`（FreeMarker・`parent=base`）で
描画する。BFF の OIDC（PKCE・state・backchannel logout）と Keycloak 側のブルートフォース検知・
WebAuthn・OTP をそのまま使うため、ログイン処理を Next.js に持ち込まず見た目だけを差し替えている。

- dev compose は `themes/shiki` を `/opt/keycloak/themes/shiki` に読み取り専用でマウントする。
  **本番では** テーマを jar（`META-INF/keycloak-themes.json`）にして `providers/` に置くか、
  イメージに焼き込むこと。
- realm は `loginTheme = shiki`、`defaultLocale = ja`。
- ブラウザフロー `shiki browser` = Cookie → IdP リダイレクタ →（パスワード ＋ 設定済みなら OTP）
  **または** パスキー（`webauthn-authenticator-passwordless`）。ログイン画面の
  「パスキーでログイン」は後者の execution へ切り替える。
- パスキー・OTP・パスワードの追加/変更/削除は Application Initiated Action（`kc_action`）で
  Keycloak の画面に任せ、BFF の `/auth/action` → `/auth/callback` で設定画面へ戻す。

## Account API（設定 → ログイン方法・端末とセッション）

BFF はユーザー本人のアクセストークンで `/realms/shiki/account/*`（credentials・sessions）を呼ぶ。
トークンに `aud=account` と `manage-account` ロールが載っている必要があるため、ユーザーには
`default-roles-shiki`（`account` client の `view-profile` / `manage-account` を含む）を付ける。
本番 realm でも同じ前提を満たすこと。
