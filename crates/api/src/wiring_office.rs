//! Office 統合（Collabora・WOPI・AI ライブ編集）の配線（Task 11.5/11.6・issue #352）。
//! main の起動フローから切り出す（1 ファイル行数規約・wiring_gui/wiring_gateway と同じ流儀）。

use std::sync::Arc;

use anyhow::Context;
use authz::AuthzClient;

use api::config::AppConfig;

/// Office 統合（Task 11.5/11.6）の配線。`office.enabled=false` なら `None`
/// （/office/sessions も /wopi も配線されない）。
///
/// トークン署名鍵は設定注入（`SHIKI__OFFICE__TOKEN_SECRET`）が無ければ起動時に
/// 乱数生成する（再起動で編集セッション失効＝許容。複数レプリカでは注入が必須）。
pub(crate) fn wire_office(
    config: &AppConfig,
    http: &reqwest::Client,
    db: &sqlx::PgPool,
    authz: &Arc<dyn AuthzClient>,
    storage: &Arc<storage::StorageService>,
) -> anyhow::Result<Option<api::state::OfficeRuntime>> {
    if !config.office.enabled {
        tracing::info!("office.enabled=false: Office 統合は無効（/office・/wopi は配線されない）");
        return Ok(None);
    }
    let base_url = config
        .office
        .collabora_base_url
        .as_deref()
        .context("office.collabora_base_url が未設定です（office.enabled=true では必須）")?;
    // Collabora から見た shiki-server のベース URL。ブラウザ側では知り得ないため
    // 推定フォールバックはせず、enabled 時は必須とする（fail-closed）。
    let wopi_base_url = config
        .office
        .wopi_base_url
        .as_deref()
        .context("office.wopi_base_url が未設定です（office.enabled=true では必須・例 http://shiki-server:8080）")?
        .trim_end_matches('/')
        .to_string();
    let token_key = if let Some(secret) = &config.office.token_secret {
        office::OfficeTokenKey::from_secret(secret)
            .map_err(|e| anyhow::anyhow!("office.token_secret が不正です: {e}"))?
    } else {
        tracing::warn!(
            "office.token_secret 未設定: 起動時乱数鍵を使用（再起動で編集セッション失効・複数レプリカ構成では設定注入が必須）"
        );
        office::OfficeTokenKey::random()
    };
    let suite: Arc<dyn office::OfficeSuite> =
        Arc::new(office::CollaboraSuite::new(base_url, http.clone()));
    let wopi = office::WopiState {
        storage: Arc::clone(storage),
        authz: Arc::clone(authz),
        pool: db.clone(),
        token_key: token_key.clone(),
        web_origin: config.office.web_origin.clone(),
        // PutFile 本文上限はアップロード上限と同一ポリシー（storage 側でも再検証される）。
        max_body_bytes: usize::try_from(config.storage.max_upload_size_bytes)
            .context("storage.max_upload_size_bytes が不正です")?,
    };
    // AI ライブ編集（office.live_edit・CoolWSD headless 参加・issue #352）。
    // WS ベース URL は collabora_base_url の http(s)→ws(s) 置換（fail-closed）。
    let ws_base = if let Some(rest) = base_url.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base_url.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        anyhow::bail!("office.collabora_base_url は http(s):// で始まる必要があります: {base_url}");
    };
    let live = Arc::new(office::live::LiveEditor::new(
        Arc::clone(storage),
        Arc::clone(authz),
        db.clone(),
        token_key,
        wopi_base_url.clone(),
        office::live::CoolWsConfig::new(ws_base),
    ));
    tracing::info!(%base_url, %wopi_base_url, "Office 統合を配線しました（Collabora・WOPI ホスト・AI ライブ編集）");
    Ok(Some(api::state::OfficeRuntime {
        suite,
        wopi,
        wopi_base_url,
        live,
    }))
}
