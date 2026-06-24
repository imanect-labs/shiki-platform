//! OpenAPI 仕様の集約（utoipa）。フロントの型生成（openapi-typescript）の入力。

use utoipa::{
    openapi::security::{ApiKey, ApiKeyValue, SecurityScheme},
    Modify, OpenApi,
};

#[derive(OpenApi)]
#[openapi(
    info(title = "shiki API", version = "0.0.0", description = "shiki-platform API"),
    paths(
        crate::routes::me::get_me,
        crate::routes::files::begin_upload,
        crate::routes::files::finalize_upload,
        crate::routes::files::download_url,
        crate::routes::files::get_file,
        crate::routes::files::update_file,
        crate::routes::files::delete_file,
        crate::routes::files::restore_file,
        crate::routes::folders::create_folder,
        crate::routes::folders::list_children,
        crate::routes::folders::breadcrumb,
        crate::routes::folders::update_folder,
        crate::routes::folders::delete_folder,
        crate::routes::shares::share_node,
        crate::routes::shares::unshare_node,
        crate::routes::shares::list_shares,
        crate::routes::shares::shared_with_me,
    ),
    components(schemas(
        crate::routes::me::MeResponse,
        crate::routes::files::FileResponse,
        crate::routes::files::UploadRequest,
        crate::routes::files::UploadTicketResponse,
        crate::routes::files::UpdateFileRequest,
        crate::routes::files::DownloadUrlResponse,
        crate::routes::folders::CreateFolderRequest,
        crate::routes::folders::UpdateFolderRequest,
        crate::routes::folders::ChildrenResponse,
        crate::routes::folders::CrumbResponse,
        crate::routes::shares::ShareTargetDto,
        crate::routes::shares::ShareRoleDto,
        crate::routes::shares::ShareRequest,
        crate::routes::shares::ShareEntryResponse,
    )),
    modifiers(&SecurityAddon),
)]
pub struct ApiDoc;

/// セッション Cookie 認証スキームを登録する（BFF + オパークセッション Cookie）。
struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "session",
            SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::new("shiki_session"))),
        );
    }
}

/// OpenAPI 仕様を JSON 文字列で返す（export-openapi bin と /api-docs/openapi.json 共用）。
pub fn openapi_json() -> String {
    ApiDoc::openapi()
        .to_pretty_json()
        .expect("OpenAPI JSON 生成に失敗")
}
