//! ミニアプリ・インストール台帳（Task 9.6/9.13・二重ゲート第2段の正本）。
//!
//! ゲートウェイは Bearer トークンの azp（登録 client_id）からこの行を引き、`granted_scopes`
//! （同意付与＝requested の部分集合）と所有アプリ（`app_id`）を得る。毎リクエスト突合すること
//! で同意失効（revoked / scope 縮小）が即時反映される（token の scope クレームに依存しない）。
//!
//! 本 PR（9.6）は台帳＋ゲートウェイ参照とデブ用直接作成のみ。実インストール/プロビジョン
//! （単一 Tx＋補償・Keycloak client 登録連動）は Task 9.13b（PR9）が担う。

use authz::AuthContext;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{map_db, GatewayError};

/// インストールのライフサイクル状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallStatus {
    /// 有効（呼び出し可能）。
    Active,
    /// アンインストール済み（token 有効期限内でも 403・即時失効）。
    Revoked,
}

impl InstallStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            InstallStatus::Active => "active",
            InstallStatus::Revoked => "revoked",
        }
    }

    fn parse(s: &str) -> InstallStatus {
        // 未知値は fail-closed で revoked 扱い（有効化に倒さない）。
        match s {
            "active" => InstallStatus::Active,
            _ => InstallStatus::Revoked,
        }
    }
}

/// AI ガードレールの同意時ピン（Task 9.9・PR9 の同意フローがマニフェスト Budget/tools から焼き込む）。
///
/// 実行時は registry/artifact を読まず**この行だけ**が効く＝ユーザーが同意した内容が上限。
#[derive(Debug, Clone, Default, Serialize)]
pub struct AiPin {
    /// 使用可能モデル（空＝テナントカタログ全体を許可）。
    pub budget_models: Vec<String>,
    /// 日次コスト上限（マイクロ USD・None＝管理者キャップのみ）。
    pub budget_daily_usd_micros: Option<i64>,
    /// 1 回の呼び出しの最大トークン。
    pub budget_max_tokens: Option<i64>,
    /// agent.invoke で提示してよい宣言ツール（ToolName 閉集合へ実行時照合）。
    pub agent_tools: Vec<String>,
}

/// インストール台帳の 1 行。
#[derive(Debug, Clone, Serialize)]
pub struct AppInstallation {
    pub id: Uuid,
    pub app_id: Uuid,
    pub app_name: String,
    pub installed_version: String,
    /// 同意で付与されたスコープ（二重ゲートの scope 上限・requested の部分集合）。
    pub granted_scopes: Vec<String>,
    pub client_id_b1: Option<String>,
    pub client_id_b2: Option<String>,
    #[serde(skip)]
    pub status: InstallStatus,
    pub installed_by: String,
    pub created_at: DateTime<Utc>,
    /// AI ガードレールの同意時ピン（Task 9.9）。
    pub ai: AiPin,
    /// B1 フロントバンドルの同意時ピン（sha256 hex・Task 9.11・None=フロントなし）。
    pub frontend_bundle: Option<String>,
    /// B2 サーバコードバンドルの同意時ピン（sha256 hex・Task 9.12・None=サーバなし）。
    pub server_bundle: Option<String>,
    /// ServerSpec 全体の同意時ピン（JSON・app-platform が解釈する。gateway は不透明に運ぶ）。
    pub server_spec: Option<serde_json::Value>,
    /// Keycloak にアプリのクライアントが無く、起動時の照合が作り直せなかった（入れ直しが要る・GAP-70・migration 0144）。
    pub oauth_client_missing: bool,
    /// Keycloak で B2 のクライアントが無効にされていて、起動時の照合が有効に戻さなかった（BUG-201・D-137・
    /// migration 0148）。
    pub oauth_client_disabled: bool,
}

/// インストール作成の入力（PR9 の同意フロー・本 PR はデブ用 fixture）。
#[derive(Debug, Clone)]
pub struct NewAppInstallation<'a> {
    pub app_id: Uuid,
    pub app_name: &'a str,
    pub installed_version: &'a str,
    pub granted_scopes: &'a [String],
    pub client_id_b1: Option<&'a str>,
    pub client_id_b2: Option<&'a str>,
    /// AI ガードレールの同意時ピン（Task 9.9・既定＝管理者キャップのみ）。
    pub ai: AiPin,
    /// B1 フロントバンドルの同意時ピン（Task 9.11）。
    pub frontend_bundle: Option<&'a str>,
    /// B2 サーバコードバンドルの同意時ピン（Task 9.12）。
    pub server_bundle: Option<&'a str>,
    /// ServerSpec 全体の同意時ピン（Task 9.12）。
    pub server_spec: Option<serde_json::Value>,
}

/// sqlx 実行時マップ用の生行（`FromRow`）。status は文字列で受けて enum へ写す。
#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    app_id: Uuid,
    app_name: String,
    installed_version: String,
    granted_scopes: Vec<String>,
    client_id_b1: Option<String>,
    client_id_b2: Option<String>,
    status: String,
    installed_by: String,
    created_at: DateTime<Utc>,
    budget_models: Vec<String>,
    budget_daily_usd_micros: Option<i64>,
    budget_max_tokens: Option<i64>,
    agent_tools: Vec<String>,
    frontend_bundle: Option<String>,
    server_bundle: Option<String>,
    server_spec: Option<serde_json::Value>,
    oauth_client_missing: bool,
    oauth_client_disabled: bool,
}

impl From<Row> for AppInstallation {
    fn from(r: Row) -> Self {
        AppInstallation {
            id: r.id,
            app_id: r.app_id,
            app_name: r.app_name,
            installed_version: r.installed_version,
            granted_scopes: r.granted_scopes,
            client_id_b1: r.client_id_b1,
            client_id_b2: r.client_id_b2,
            status: InstallStatus::parse(&r.status),
            installed_by: r.installed_by,
            created_at: r.created_at,
            ai: AiPin {
                budget_models: r.budget_models,
                budget_daily_usd_micros: r.budget_daily_usd_micros,
                budget_max_tokens: r.budget_max_tokens,
                agent_tools: r.agent_tools,
            },
            frontend_bundle: r.frontend_bundle,
            server_bundle: r.server_bundle,
            server_spec: r.server_spec,
            oauth_client_missing: r.oauth_client_missing,
            oauth_client_disabled: r.oauth_client_disabled,
        }
    }
}

/// 全列（RETURNING / SELECT 共通・Row の FromRow と対応）。
const COLS: &str = "id, app_id, app_name, installed_version, granted_scopes, \
                    client_id_b1, client_id_b2, status, installed_by, created_at, \
                    budget_models, budget_daily_usd_micros, budget_max_tokens, agent_tools, \
                    frontend_bundle, server_bundle, server_spec, oauth_client_missing, \
                    oauth_client_disabled";

/// active な app_id の大域一意索引（migration 0081・B1 配信のテナント跨ぎ解決の前提）。
const ACTIVE_APP_ID_UIDX: &str = "app_installation_active_app_id_uidx";

/// [`ACTIVE_APP_ID_UIDX`] の一意制約違反か（他の制約の違反は従来どおり Internal に落とす）。
fn violates_active_app_id(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db)
        if db.is_unique_violation() && db.constraint() == Some(ACTIVE_APP_ID_UIDX))
}

/// 別テナントに同じ app_id の active 行がある＝B1 配信の解決先が曖昧になるため拒否する。
fn active_app_id_conflict() -> GatewayError {
    GatewayError::Conflict("同じ app_id の有効なインストールが別テナントに存在します".into())
}

/// 同じテナントのほかの組織に、このアプリの有効なインストールがあるときの文言（利用者に見せる・BUG-212）。
///
/// api はこの文言の 409 だけを、`installed_in_other_org` の code と文言つきで返す（ほかの 409 の文言は内部の詳細を
/// 含みうるので返さない）。文言はここだけに書く。
pub const INSTALLED_IN_OTHER_ORG: &str = "このアプリはほかの組織でインストールされています。インストールとアンインストールは、インストールした組織で行ってください。";

/// 同じテナントのほかの組織に、このアプリの有効なインストールがある（入れ直し・アンインストールを断る・BUG-212）。
///
/// インストールはテナントに 1 アプリ 1 行で、入れた組織のもの（組織はテナントの中の隔離の境界・PIT-45）。ほかの組織の
/// 同意（スコープ・クライアント・予算）をこの組織の操作で書き換えない・外さない。
fn installed_in_other_org() -> GatewayError {
    GatewayError::Conflict(INSTALLED_IN_OTHER_ORG.into())
}

/// インストール台帳ストア（Postgres・tenant スコープ）。
#[derive(Clone)]
pub struct AppInstallationStore {
    db: PgPool,
}

impl AppInstallationStore {
    pub fn new(db: PgPool) -> Self {
        AppInstallationStore { db }
    }

    /// インストールを作成/更新する（tenant 内 1 アプリ 1 行・再インストールは upsert）。
    ///
    /// 本 PR ではデブ用の直接作成。PR9 の同意フローが単一 Tx＋補償で置き換える。
    ///
    /// 組織で絞る（PIT-45・BUG-212）: 同じ組織の行だけを書き換える。ほかの組織の行が有効なら書き換えずに 409
    /// （[`installed_in_other_org`]）。ほかの組織の行がアンインストール済み（revoked）なら、この組織のインストールとして
    /// 入れ直す（組織と入れた人も書き換える。アンインストールでテーブル・購読・スケジュール・クライアントは片付いている）。
    ///
    /// ON CONFLICT の arbiter は `(tenant_id, app_id)` だけで、active な app_id の大域一意索引
    /// （migration 0081 [`ACTIVE_APP_ID_UIDX`]）は arbiter ではない。そのため同一テナントの初回
    /// インストールが並行すると、後着は arbiter の事前確認をすり抜けて大域一意索引で一意制約違反に
    /// なる。自テナントの行（先着分）があるときだけ 1 回取り直し、DO UPDATE で冪等に成功させる。
    /// 取り直しても違反する・自テナントの行が無いなら、別テナントの active 行によるもの＝409。
    pub async fn upsert(
        &self,
        ctx: &AuthContext,
        new: NewAppInstallation<'_>,
    ) -> Result<AppInstallation, GatewayError> {
        let row = match self.try_upsert(ctx, &new).await {
            Ok(row) => row,
            Err(e) if violates_active_app_id(&e) => {
                if !self.exists_in_tenant(ctx, new.app_id).await? {
                    return Err(active_app_id_conflict());
                }
                self.try_upsert(ctx, &new).await.map_err(|e| {
                    if violates_active_app_id(&e) {
                        active_app_id_conflict()
                    } else {
                        map_db(e)
                    }
                })?
            }
            Err(e) => return Err(map_db(e)),
        };
        // 行を返さない＝ほかの組織の有効なインストール（DO UPDATE の WHERE で書き換えなかった）。
        let row = row.ok_or_else(installed_in_other_org)?;
        Ok(row.into())
    }

    /// 同じテナントのほかの組織に、このアプリの有効なインストールがあれば 409（[`installed_in_other_org`]）。
    ///
    /// インストールの前に、テーブルやクライアントを作る前に断るための確かめ（書き込みの守りは [`Self::upsert`] が行う）。
    pub async fn ensure_not_in_other_org(
        &self,
        ctx: &AuthContext,
        app_id: Uuid,
    ) -> Result<(), GatewayError> {
        if self.active_in_other_org(ctx, app_id).await? {
            return Err(installed_in_other_org());
        }
        Ok(())
    }

    /// 同じテナントのほかの組織に、このアプリの有効なインストールがあるか。
    async fn active_in_other_org(
        &self,
        ctx: &AuthContext,
        app_id: Uuid,
    ) -> Result<bool, GatewayError> {
        let (exists,): (bool,) = sqlx::query_as(
            "SELECT EXISTS (SELECT 1 FROM app_installation \
             WHERE tenant_id = $1 AND app_id = $2 AND org <> $3 AND status = 'active')",
        )
        .bind(ctx.tenant_id())
        .bind(app_id)
        .bind(ctx.org())
        .fetch_one(&self.db)
        .await
        .map_err(map_db)?;
        Ok(exists)
    }

    /// upsert の 1 回分（エラーの解釈は呼び出し側）。
    async fn try_upsert(
        &self,
        ctx: &AuthContext,
        new: &NewAppInstallation<'_>,
    ) -> Result<Option<Row>, sqlx::Error> {
        let sql = format!(
            "INSERT INTO app_installation \
                 (tenant_id, org, app_id, app_name, installed_version, granted_scopes, \
                  client_id_b1, client_id_b2, status, installed_by, \
                  budget_models, budget_daily_usd_micros, budget_max_tokens, agent_tools, \
                  frontend_bundle, server_bundle, server_spec) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'active', $9, $10, $11, $12, $13, $14, $15, $16) \
             ON CONFLICT (tenant_id, app_id) DO UPDATE SET \
                 org = EXCLUDED.org, \
                 installed_by = CASE WHEN app_installation.org = EXCLUDED.org \
                     THEN app_installation.installed_by ELSE EXCLUDED.installed_by END, \
                 app_name = EXCLUDED.app_name, \
                 installed_version = EXCLUDED.installed_version, \
                 granted_scopes = EXCLUDED.granted_scopes, \
                 client_id_b1 = EXCLUDED.client_id_b1, \
                 client_id_b2 = EXCLUDED.client_id_b2, \
                 budget_models = EXCLUDED.budget_models, \
                 budget_daily_usd_micros = EXCLUDED.budget_daily_usd_micros, \
                 budget_max_tokens = EXCLUDED.budget_max_tokens, \
                 agent_tools = EXCLUDED.agent_tools, \
                 frontend_bundle = EXCLUDED.frontend_bundle, \
                 server_bundle = EXCLUDED.server_bundle, \
                 server_spec = EXCLUDED.server_spec, \
                 status = 'active', \
                 updated_at = now() \
             WHERE app_installation.org = EXCLUDED.org OR app_installation.status <> 'active' \
             RETURNING {COLS}"
        );
        sqlx::query_as(&sql)
            .bind(ctx.tenant_id())
            .bind(ctx.org())
            .bind(new.app_id)
            .bind(new.app_name)
            .bind(new.installed_version)
            .bind(new.granted_scopes)
            .bind(new.client_id_b1)
            .bind(new.client_id_b2)
            .bind(&ctx.principal().id)
            .bind(&new.ai.budget_models)
            .bind(new.ai.budget_daily_usd_micros)
            .bind(new.ai.budget_max_tokens)
            .bind(&new.ai.agent_tools)
            .bind(new.frontend_bundle)
            .bind(new.server_bundle)
            .bind(&new.server_spec)
            .fetch_optional(&self.db)
            .await
    }

    /// 自テナントに app_id の行（status を問わない）があるか（upsert の取り直し判定）。
    async fn exists_in_tenant(
        &self,
        ctx: &AuthContext,
        app_id: Uuid,
    ) -> Result<bool, GatewayError> {
        let (exists,): (bool,) = sqlx::query_as(
            "SELECT EXISTS (SELECT 1 FROM app_installation WHERE tenant_id = $1 AND app_id = $2)",
        )
        .bind(ctx.tenant_id())
        .bind(app_id)
        .fetch_one(&self.db)
        .await
        .map_err(map_db)?;
        Ok(exists)
    }

    /// azp（登録 client_id）から**有効な**インストールと、それを入れた組織を引く（ゲートウェイの二重ゲート第2段）。
    ///
    /// B1/B2 いずれの client_id でも解決する。revoked / 不在は `None`（fail-closed）。組織は、ゲートウェイが利用者の
    /// 組織として使う（トークンの所属グループでは決めない・D-130）。
    pub async fn resolve_active_by_client(
        &self,
        tenant_id: &str,
        client_id: &str,
    ) -> Result<Option<ResolvedInstallation>, GatewayError> {
        let sql = format!(
            "SELECT tenant_id AS service_tenant_id, org AS service_org, {COLS} FROM app_installation \
             WHERE tenant_id = $1 AND status = 'active' \
               AND (client_id_b1 = $2 OR client_id_b2 = $2) \
             LIMIT 1"
        );
        let row: Option<ServiceRow> = sqlx::query_as(&sql)
            .bind(tenant_id)
            .bind(client_id)
            .fetch_optional(&self.db)
            .await
            .map_err(map_db)?;
        Ok(row.map(Into::into))
    }

    /// B2 のサービスアカウントのトークン（`azp` = `client_id`・`sub` = `sub`）から、そのアプリの**有効な**
    /// インストールと、それを入れたテナント・組織を引く（自動実行のアプリの主体・BUG-183）。
    ///
    /// 一致するのは、インストールの B2 のクライアントと、そのサービスアカウントの利用者（`b2_service_sub`・
    /// migration 0143）の両方が合うトークンだけ。同じ B2 が交換した利用者のトークン（`sub` は利用者）・別のアプリの
    /// クライアントのトークン・アンインストールしたアプリのトークンは `None`（利用者として扱うか、断る）。
    ///
    /// **テナントで絞らない理由**: サービスアカウントの利用者には tenant の属性が無く、トークンにテナントが無い。
    /// テナントと組織は、トークンのクレームではなく、この行から決める（クライアントの id は realm で一意・有効な
    /// app_id は大域で一意・migration 0081）。2 行返れば不変条件の違反として fail-closed にする。
    pub async fn resolve_service_identity(
        &self,
        client_id: &str,
        sub: &str,
    ) -> Result<Option<ResolvedInstallation>, GatewayError> {
        let sql = format!(
            "SELECT tenant_id AS service_tenant_id, org AS service_org, {COLS} FROM app_installation \
             WHERE b2_service_sub = $1 AND client_id_b2 = $2 AND status = 'active' LIMIT 2"
        );
        let rows: Vec<ServiceRow> = sqlx::query_as(&sql)
            .bind(sub)
            .bind(client_id)
            .fetch_all(&self.db)
            .await
            .map_err(map_db)?;
        match <[ServiceRow; 1]>::try_from(rows) {
            Ok([row]) => Ok(Some(row.into())),
            Err(rows) if rows.is_empty() => Ok(None),
            Err(_) => {
                tracing::error!(
                    client_id,
                    "B2 のサービスアカウントの解決先が一意ではありません"
                );
                Err(GatewayError::Internal(
                    "インストールの解決先が一意ではありません".into(),
                ))
            }
        }
    }

    /// app_id から**有効な**インストールを引く（アンインストール時の client 特定など）。
    pub async fn resolve_active_by_app(
        &self,
        tenant_id: &str,
        app_id: Uuid,
    ) -> Result<Option<AppInstallation>, GatewayError> {
        let sql = format!(
            "SELECT {COLS} FROM app_installation \
             WHERE tenant_id = $1 AND app_id = $2 AND status = 'active' LIMIT 1"
        );
        let row: Option<Row> = sqlx::query_as(&sql)
            .bind(tenant_id)
            .bind(app_id)
            .fetch_optional(&self.db)
            .await
            .map_err(map_db)?;
        Ok(row.map(Into::into))
    }

    /// app_id から**有効な**インストールを引く（テナント跨ぎ・B1 配信用）。
    ///
    /// **テナントで絞れない理由**: B1 第3リスナ（`/a/{app_id}/{sha256}`）は cookie/token を持たず
    /// URL にもテナントを含まない（opaque origin・cookieless 配信・iframe の `src` がそのまま
    /// immutable キャッシュの鍵）。呼び出し文脈にテナントが無いので、`app_id` から active
    /// インストールと所属テナントを解決し、返った `tenant_id` でバンドルキー（tenant スコープの
    /// content address）を組む。仮に URL にテナントを載せても未認証の入力でしかなく、認可の
    /// 根拠にはならない。
    ///
    /// **一意性**: active な `app_id` の大域一意は DB の部分一意索引で保証する（migration 0081
    /// `app_installation_active_app_id_uidx`）。それでも 2 行返れば不変条件違反として
    /// fail-closed（Internal）にし、任意の 1 行を選ぶ `LIMIT 1` には戻さない。
    /// バンドルはアプリ自身のフロント資産（content-addressed・ピン一致必須）でユーザーデータを
    /// 含まないため、この解決でテナント隔離は損なわれない。
    pub async fn resolve_active_by_app_global(
        &self,
        app_id: Uuid,
    ) -> Result<Option<(String, AppInstallation)>, GatewayError> {
        // tenant を先に解決してから tenant スコープの取得へ（Row の FromRow を汚さない）。
        // LIMIT 2 で「複数該当」を検知できる最小限だけ読む。
        let tenants: Vec<(String,)> = sqlx::query_as(
            "SELECT tenant_id FROM app_installation \
             WHERE app_id = $1 AND status = 'active' LIMIT 2",
        )
        .bind(app_id)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        let tenant_id = match tenants.as_slice() {
            [] => return Ok(None),
            [(tenant_id,)] => tenant_id.clone(),
            _ => {
                tracing::error!(%app_id, "active な app_id が複数テナントに存在（不変条件違反）");
                return Err(GatewayError::Internal(
                    "インストールの解決先が一意ではありません".into(),
                ));
            }
        };
        let installation = self.resolve_active_by_app(&tenant_id, app_id).await?;
        Ok(installation.map(|i| (tenant_id, i)))
    }

    /// 呼ぶ人の組織の active インストール一覧（インストール済みアプリ一覧 UI 用・新しい順）。
    ///
    /// アンインストール（status=revoked）は起動不能なため一覧から除く。撤去済みアプリが
    /// シェル一覧・B1 実行画面に残らないよう active に限定する（失効の即時反映）。
    ///
    /// テナントと組織の両方で絞る（組織はテナントの中の隔離の境界・PIT-45 の単一定義・BUG-197）。別の組織が入れた
    /// アプリの名前・スコープ・クライアント・入れた人・関数の定義は見せない（開いてもゲートウェイが断る・D-130）。
    pub async fn list(&self, ctx: &AuthContext) -> Result<Vec<AppInstallation>, GatewayError> {
        let sql = format!(
            "SELECT {COLS} FROM app_installation \
             WHERE tenant_id = $1 AND org = $2 AND status = 'active' \
             ORDER BY created_at DESC LIMIT 200"
        );
        let rows: Vec<Row> = sqlx::query_as(&sql)
            .bind(ctx.tenant_id())
            .bind(ctx.org())
            .fetch_all(&self.db)
            .await
            .map_err(map_db)?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// インストールを失効させる（アンインストール・即時反映）。
    ///
    /// 呼ぶ人の組織のインストールだけ（PIT-45・BUG-212）。この組織に無く、ほかの組織に有効なインストールがあれば 409
    /// （[`installed_in_other_org`]）、どこにも無ければ 404。
    pub async fn revoke(&self, ctx: &AuthContext, app_id: Uuid) -> Result<(), GatewayError> {
        let updated = sqlx::query(&format!(
            "UPDATE app_installation SET status = '{}', updated_at = now() \
             WHERE tenant_id = $1 AND app_id = $2 AND org = $3",
            InstallStatus::Revoked.as_str()
        ))
        .bind(ctx.tenant_id())
        .bind(app_id)
        .bind(ctx.org())
        .execute(&self.db)
        .await
        .map_err(map_db)?;
        if updated.rows_affected() == 0 {
            if self.active_in_other_org(ctx, app_id).await? {
                return Err(installed_in_other_org());
            }
            return Err(GatewayError::NotFound);
        }
        Ok(())
    }

    /// 有効なインストールのうち、クライアントを `fingerprint` の登録の形で揃えていないものと、前の照合でクライアントを
    /// 作り直せなかったもの（`oauth_client_missing`・GAP-70）・無効の B2 を有効に戻さなかったもの
    /// （`oauth_client_disabled`・BUG-201）を、古い順に最大 `limit` 件引く（起動時の照合・BUG-172・migration 0141）。
    /// `skip`（この照合で揃えた・揃えられなかったアプリ）は外す。
    ///
    /// **テナントで絞らない理由**: 起動時の保守の走査で、呼び出す人がいない（[`Self::resolve_active_by_app_global`]
    /// と同じく文脈にテナントが無い）。読むのはクライアントを揃えるための列だけで、利用者のデータは読まない。
    pub async fn clients_to_repair(
        &self,
        fingerprint: &str,
        skip: &[Uuid],
        limit: i64,
    ) -> Result<Vec<ClientsToRepair>, GatewayError> {
        sqlx::query_as(
            "SELECT tenant_id, org, app_id, app_name, client_id_b1, client_id_b2 FROM app_installation \
             WHERE status = 'active' \
               AND (client_id_b1 IS NOT NULL OR client_id_b2 IS NOT NULL) \
               AND (oauth_client_spec IS DISTINCT FROM $1 OR oauth_client_missing \
                    OR oauth_client_disabled) \
               AND NOT (app_id = ANY($2)) \
             ORDER BY created_at, id LIMIT $3",
        )
        .bind(fingerprint)
        .bind(skip)
        .bind(limit)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)
    }

    /// インストールのクライアントを `fingerprint` の登録の形で揃えた印を付ける（インストールと起動時の照合）。
    ///
    /// あわせて、B2 のサービスアカウントの利用者（`b2_service_sub`・B2 が無い・使えなければ `None`・BUG-183）と、
    /// 使えないクライアントの印（[`ClientMarks`]・GAP-70・BUG-201）を書く。
    pub async fn mark_clients_repaired(
        &self,
        tenant_id: &str,
        app_id: Uuid,
        fingerprint: &str,
        b2_service_sub: Option<&str>,
        marks: ClientMarks,
    ) -> Result<(), GatewayError> {
        sqlx::query(
            "UPDATE app_installation \
             SET oauth_client_spec = $3, b2_service_sub = $4, oauth_client_missing = $5, \
                 oauth_client_disabled = $6 \
             WHERE tenant_id = $1 AND app_id = $2",
        )
        .bind(tenant_id)
        .bind(app_id)
        .bind(fingerprint)
        .bind(b2_service_sub)
        .bind(marks.missing)
        .bind(marks.disabled)
        .execute(&self.db)
        .await
        .map_err(map_db)?;
        Ok(())
    }

    /// 有効でないインストールから、Keycloak から消したクライアントの id を外す（もう入れられないアプリの片付け・
    /// GAP-69a）。
    ///
    /// 外すのは、渡した id と同じ値の列だけ（`b1`・`b2` が `None` か、別の値ならその列は残す）。有効なインストール
    /// （同じ id で入れ直したもの）は書き換えない。外した行は、起動時の片付けの対象から外れる（2 回目からの起動では
    /// Keycloak を呼ばない）。
    pub async fn forget_clients(
        &self,
        tenant_id: &str,
        app_id: Uuid,
        b1: Option<&str>,
        b2: Option<&str>,
    ) -> Result<(), GatewayError> {
        sqlx::query(&format!(
            "UPDATE app_installation \
             SET client_id_b1 = CASE WHEN client_id_b1 = $3 THEN NULL ELSE client_id_b1 END, \
                 client_id_b2 = CASE WHEN client_id_b2 = $4 THEN NULL ELSE client_id_b2 END, \
                 updated_at = now() \
             WHERE tenant_id = $1 AND app_id = $2 AND status <> '{}'",
            InstallStatus::Active.as_str()
        ))
        .bind(tenant_id)
        .bind(app_id)
        .bind(b1)
        .bind(b2)
        .execute(&self.db)
        .await
        .map_err(map_db)?;
        Ok(())
    }
}

/// アプリのクライアントが使えない印（[`AppInstallationStore::mark_clients_repaired`]・インストールの画面が知らせる）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClientMarks {
    /// Keycloak にクライアントが無く、作り直さなかった・作り直せなかった（入れ直しが要る・GAP-70）。
    pub missing: bool,
    /// Keycloak で B2 のクライアントが無効にされていて、有効に戻さなかった（BUG-201・D-137）。
    pub disabled: bool,
}

/// トークンが指すインストールと、それを入れたテナント・組織（[`AppInstallationStore::resolve_active_by_client`]・
/// [`AppInstallationStore::resolve_service_identity`]）。
///
/// ゲートウェイは、利用者の経路でもアプリの主体（自動実行）の経路でも、組織をここから決める（トークンのクレームでは
/// 決めない・BUG-183・D-130）。
#[derive(Debug, Clone)]
pub struct ResolvedInstallation {
    /// インストールしたテナント。
    pub tenant_id: String,
    /// インストールした組織。
    pub org: String,
    pub installation: AppInstallation,
}

/// [`ResolvedInstallation`] の生行（テナント・組織と、[`Row`] の全列）。
#[derive(sqlx::FromRow)]
struct ServiceRow {
    service_tenant_id: String,
    service_org: String,
    #[sqlx(flatten)]
    row: Row,
}

impl From<ServiceRow> for ResolvedInstallation {
    fn from(row: ServiceRow) -> Self {
        ResolvedInstallation {
            tenant_id: row.service_tenant_id,
            org: row.service_org,
            installation: row.row.into(),
        }
    }
}

/// クライアントを今の登録の形に揃えるインストール 1 件（[`AppInstallationStore::clients_to_repair`]）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ClientsToRepair {
    pub tenant_id: String,
    /// インストールした組織（B2 の secret をアプリの主体で読む・GAP-70）。
    pub org: String,
    pub app_id: Uuid,
    pub app_name: String,
    pub client_id_b1: Option<String>,
    pub client_id_b2: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_roundtrip_and_fail_closed() {
        assert_eq!(InstallStatus::Active.as_str(), "active");
        assert_eq!(InstallStatus::Revoked.as_str(), "revoked");
        assert_eq!(InstallStatus::parse("active"), InstallStatus::Active);
        assert_eq!(InstallStatus::parse("revoked"), InstallStatus::Revoked);
        // 未知値は fail-closed（revoked 扱い）。
        assert_eq!(InstallStatus::parse("bogus"), InstallStatus::Revoked);
        assert_eq!(InstallStatus::parse(""), InstallStatus::Revoked);
    }
}
