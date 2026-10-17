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
    // Excel のレイアウト編集エンジン（#725）。Collabora の版（executescript の有無）は初回に確かめる。
    let engine = Arc::new(office::engine::OfficeEngine::new(
        Arc::clone(&live),
        http.clone(),
        base_url,
    ));
    tracing::info!(%base_url, %wopi_base_url, "Office 統合を配線しました（Collabora・WOPI ホスト・AI ライブ編集・エンジン）");
    Ok(Some(api::state::OfficeRuntime {
        suite,
        wopi,
        wopi_base_url,
        live,
        engine,
    }))
}

/// ブラウザ ↔ Collabora の中継（PIT-68）を spawn する。office 無効・`relay_port=0` では何もしない。
///
/// ブラウザ向けの面なので、内部 API と同じ host（`server.host`）で待ち受ける。
/// 上流の 3xx をブラウザへそのまま返すため、リダイレクトを追わない専用クライアントを使う。
pub(crate) async fn spawn_office_relay(config: &AppConfig) -> anyhow::Result<()> {
    if !config.office.enabled || config.office.relay_port == 0 {
        if config.office.enabled {
            tracing::warn!(
                "office.relay_port=0: Collabora の中継を起動しません（Collabora を別の中継の裏に置き、スクリプト実行命令を落とすこと・PIT-68）"
            );
        }
        return Ok(());
    }
    let upstream_base = config
        .office
        .collabora_base_url
        .clone()
        .context("office.collabora_base_url が未設定です（office.enabled=true では必須）")?;
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(10))
        // 応答の途中で止まった上流を待ち続けない（本文はストリームで返すので、読み取りの間隔で切る）。
        .read_timeout(std::time::Duration::from_mins(2))
        .build()
        .context("office 中継の HTTP クライアントを構築できません")?;
    let router = office::relay::build_relay_router(
        office::relay::RelayConfig {
            upstream_base: upstream_base.clone(),
            // アップロード上限と同じ（画像の挿入・貼り付けが通る大きさ）。
            max_body_bytes: usize::try_from(config.storage.max_upload_size_bytes)
                .context("storage.max_upload_size_bytes が不正です")?,
        },
        http,
    );
    let bind = format!("{}:{}", config.server.host, config.office.relay_port);
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .with_context(|| format!("office 中継の bind に失敗しました（{bind}）"))?;
    tracing::info!(%bind, %upstream_base, "office 中継を起動しました（ブラウザ ↔ Collabora）");
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router).await {
            tracing::error!(error = %e, "office 中継が停止しました");
        }
    });
    Ok(())
}
