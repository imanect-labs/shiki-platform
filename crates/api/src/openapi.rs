//! OpenAPI 仕様の集約（utoipa）。フロントの型生成（openapi-typescript）の入力。

use utoipa::{
    openapi::security::{ApiKey, ApiKeyValue, HttpAuthScheme, HttpBuilder, SecurityScheme},
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
        crate::routes::files::list_versions,
        crate::routes::files::version_download_url,
        crate::routes::files::restore_version,
        crate::routes::folders::create_folder,
        crate::routes::folders::list_children,
        crate::routes::folders::breadcrumb,
        crate::routes::folders::update_folder,
        crate::routes::folders::delete_folder,
        crate::routes::folders::restore_folder,
        crate::routes::folders::list_trash,
        crate::routes::shares::share_node,
        crate::routes::shares::unshare_node,
        crate::routes::shares::list_shares,
        crate::routes::shares::shared_with_me,
        crate::routes::directory::search_users,
        crate::routes::directory::search_roles,
        crate::routes::search::search,
        crate::routes::chat::create_thread,
        crate::routes::chat::list_threads,
        crate::routes::chat::get_thread,
        crate::routes::chat::get_messages,
        crate::routes::chat::post_message,
        crate::routes::chat::stream_thread,
        crate::routes::chat::cancel_run,
        crate::routes::chat::share_thread,
        crate::routes::chat::unshare_thread,
        crate::routes::chat::list_thread_shares,
        crate::routes::admin::create_tenant,
        crate::routes::admin::delete_tenant,
        crate::routes::artifacts::create_artifact,
        crate::routes::artifacts::list_artifacts,
        crate::routes::artifacts::get_artifact,
        crate::routes::artifacts::delete_artifact,
        crate::routes::artifacts::append_version,
        crate::routes::artifacts::list_versions,
        crate::routes::artifacts::get_version,
        crate::routes::artifacts::share_artifact,
        crate::routes::artifacts::unshare_artifact,
        crate::routes::artifacts::list_artifact_shares,
    ),
    components(schemas(
        crate::routes::me::MeResponse,
        crate::routes::files::NodeResponse,
        crate::routes::files::UploadRequest,
        crate::routes::files::UploadTicketResponse,
        crate::routes::files::UpdateFileRequest,
        crate::routes::files::DownloadUrlResponse,
        crate::routes::files::FileVersionResponse,
        crate::routes::files::FileVersionsResponse,
        crate::routes::folders::CreateFolderRequest,
        crate::routes::folders::UpdateFolderRequest,
        crate::routes::folders::ChildrenResponse,
        crate::routes::folders::CrumbResponse,
        crate::routes::folders::SortField,
        crate::routes::shares::ShareRequest,
        crate::routes::directory::DirectoryUserResponse,
        crate::routes::directory::DirectorySearchResponse,
        crate::routes::directory::DirectoryRoleResponse,
        crate::routes::directory::DirectoryRoleSearchResponse,
        crate::routes::admin::CreateTenantRequest,
        crate::routes::admin::CreateTenantResponse,
        // チャット DTO/イベント型は chat 側の単一定義（フロント chat-api.ts と同型）。
        crate::routes::chat::CreateThreadRequest,
        crate::routes::chat::ThreadListResponse,
        crate::routes::chat::MessagesResponse,
        crate::routes::chat::PostMessageRequest,
        crate::routes::chat::PostMessageResponse,
        crate::routes::chat::ShareThreadRequest,
        crate::routes::chat::ThreadShareEntry,
        crate::routes::chat::ThreadSharesResponse,
        chat::Thread,
        chat::Message,
        chat::ContentBlock,
        chat::Citation,
        chat::Attachment,
        chat::Role,
        chat::RunStatus,
        chat::ThreadRole,
        chat::StreamEvent,
        chat::StreamEventKind,
        storage::ShareTarget,
        storage::ShareRole,
        storage::ShareEntry,
        // アーティファクト DTO は artifact 側の単一定義（Task 6.1・手書きミラー禁止）。
        crate::routes::artifacts::CreateArtifactRequest,
        crate::routes::artifacts::AppendVersionRequest,
        crate::routes::artifacts::ShareArtifactRequest,
        crate::routes::artifacts::ArtifactShareEntry,
        crate::routes::artifacts::ArtifactListResponse,
        crate::routes::artifacts::VersionListResponse,
        artifact::Artifact,
        artifact::ArtifactKind,
        artifact::ArtifactRole,
        artifact::ArtifactVersion,
        artifact::VersionMeta,
        // 検索 DTO は rag 側の単一定義（手書きミラー禁止）。
        rag::SearchRequest,
        rag::SearchResponse,
        rag::SearchResult,
        rag::SearchMode,
        rag::SearchDebug,
        rag::StageTimings,
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
        // admin プレーン（/admin/*）: provisioner service account の Bearer JWT。
        components.add_security_scheme(
            "provisioner_token",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .bearer_format("JWT")
                    .build(),
            ),
        );
    }
}

/// OpenAPI 仕様を JSON 文字列で返す（export-openapi bin と /api-docs/openapi.json 共用）。
// utoipa 派生の静的仕様の JSON 直列化であり失敗はビルド時に固定される不変条件。
#[allow(clippy::expect_used)]
pub fn openapi_json() -> String {
    ApiDoc::openapi()
        .to_pretty_json()
        .expect("OpenAPI JSON 生成に失敗")
}
