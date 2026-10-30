//! ドライブのテキストの表示と編集（BUG-141・D-119）。
//!
//! 文字コードの判定をサーバーの `textenc` の 1 か所に寄せる（以前はブラウザーの TextDecoder でもう 1 つ判定を
//! 持っていた）。表示は判定して UTF-8 の文章で返し、保存は**元の文字コードと BOM のまま**書き戻す（CSV の編集と
//! 同じ）。以前の画面は Shift_JIS のファイルを BOM の無い UTF-8 で保存していたので、Excel や取り込む先の仕組みが
//! 読めなくなっていた。元の文字コードに無い文字は書かずに、分かる誤りで断る。
//!
//! 保存の文字コードは、保存する直前の版（`base_version`）の中身からサーバーが決め直す（画面が送った文字コードは
//! 信じない）。版の比べは行ロックの中で行い、その間に入った保存を上書きしない（アップロードの内容更新と同じ・#827）。
//! 認可は StorageService が持つ（表示は viewer@file・保存は editor@file）。

use axum::extract::{DefaultBodyLimit, Path, State};
use axum::routing::get;
use axum::Json;
use serde::{Deserialize, Serialize};
use storage::NodeKind;
use utoipa::ToSchema;
use uuid::Uuid;

use super::files::NodeResponse;
use crate::error::ApiError;
use crate::extract::{AuthContextExt, TraceIdExt};
use crate::server::RouteDecl;
use crate::state::AppState;

/// 表示・編集する中身の大きさの上限（画面の `TEXT_MAX_BYTES` と同じ 2 MB。全行を画面に置くので、描画が固まらない
/// 大きさに抑える）。これより大きいファイルは開かずにダウンロードを勧める。
pub const TEXT_FILE_MAX_BYTES: i64 = 2 * 1024 * 1024;

/// 保存する中身の大きさの上限（元の文字コードで書いた後のバイト数）。開ける大きさより大きくしておく（編集で
/// 書き足せるように・Shift_JIS は UTF-8 にすると大きくなる）。
const TEXT_SAVE_MAX_BYTES: usize = 8 * 1024 * 1024;

/// 保存の要求の本文の上限（[`TEXT_SAVE_MAX_BYTES`] の文章を JSON で送れる大きさ）。
const TEXT_SAVE_BODY_LIMIT: usize = TEXT_SAVE_MAX_BYTES * 2;

/// 文字コードを決められないファイルの先頭のうち、NUL を探す範囲（文章でないファイルを見せない）。
const BINARY_SNIFF_BYTES: usize = 8000;

const NOT_TEXT: &str =
    "テキストではないため表示できません。ダウンロードして、お使いのアプリで開いてください。";
const TOO_LARGE: &str =
    "2 MB を超えるテキストは表示できません。ダウンロードして、お使いのアプリで開いてください。";
const UNKNOWN_ENCODING_SAVE: &str =
    "このファイルの文字コードを決められないため、保存できません。ダウンロードして、お使いのアプリで直してください。";
const SAVE_TOO_LARGE: &str = "8 MB を超えるため保存できません。";

/// テキストのルート宣言。
pub(crate) fn file_text_route_decls() -> Vec<RouteDecl> {
    use crate::server::AccessPolicy::Session;
    vec![RouteDecl::new(
        "/files/{id}/text",
        &["GET", "PUT"],
        Session,
        || {
            get(get_text)
                .put(put_text)
                .layer(DefaultBodyLimit::max(TEXT_SAVE_BODY_LIMIT))
        },
    )]
}

/// テキストの中身（サーバーが文字コードを決めて読んだもの）。
#[derive(Debug, Serialize, ToSchema)]
pub struct TextFileResponse {
    /// 中身の文章（BOM は含めない）。文字コードを決められないときは、読めないバイトを置き換え文字にした UTF-8 として
    /// 読んだもの（表示だけ・`encoding` は `null`）。
    pub text: String,
    /// 読んだ文字コード（`UTF-8`・`Shift_JIS`・`EUC-JP`・`UTF-16LE`・`UTF-16BE`）。決められないときは `null`
    /// （編集できない）。保存はこの文字コードと BOM のまま書き戻す。
    pub encoding: Option<String>,
    /// 先頭に BOM があったか。
    pub bom: bool,
    /// 読んだ中身の版（保存の `base_version` に使う）。
    pub version: i64,
    pub size_bytes: i64,
}

/// テキストの保存（元の文字コードと BOM のまま、新しい版にする）。
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveTextFileRequest {
    /// 編集した文章。改行コードは画面が開いたときのまま送る（サーバーは変えない）。
    pub text: String,
    /// 編集の元にした版。今の版と違えば 409（`code: "version_conflict"`・`current_version` 付き）。
    pub base_version: i64,
}

/// テキストの中身を読む（viewer）。
#[utoipa::path(
    get,
    path = "/files/{id}/text",
    params(("id" = Uuid, Path, description = "ファイル ID")),
    responses(
        (status = 200, description = "中身の文章と文字コード", body = TextFileResponse),
        (status = 400, description = "テキストではない・大きすぎる（`invalid_input` と画面に出す文言）"),
        (status = 401, description = "未認証"),
        (status = 403, description = "認可されていない"),
        (status = 404, description = "ファイルが無い"),
    ),
    security(("session" = [])),
)]
pub async fn get_text(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<TextFileResponse>, ApiError> {
    // 大きさは読む前に確かめる（大きなファイルをメモリに載せない）。
    let meta = state
        .storage
        .get_metadata(&ctx, id, trace.as_deref())
        .await?;
    if meta.kind != NodeKind::File {
        return Err(ApiError::NotFound);
    }
    if meta.size_bytes.unwrap_or(0) > TEXT_FILE_MAX_BYTES {
        return Err(ApiError::invalid_input(TOO_LARGE));
    }
    let (node, bytes) = state
        .storage
        .read_file_internal(&ctx, id, trace.as_deref())
        .await?;
    // 読む間に大きな版へ替わったとき。
    if i64::try_from(bytes.len()).unwrap_or(i64::MAX) > TEXT_FILE_MAX_BYTES {
        return Err(ApiError::invalid_input(TOO_LARGE));
    }
    let size_bytes = i64::try_from(bytes.len()).unwrap_or(i64::MAX);
    match textenc::decode_text(&bytes) {
        Ok(decoded) => Ok(Json(TextFileResponse {
            text: decoded.text,
            encoding: Some(decoded.encoding.name().to_string()),
            bom: decoded.bom,
            version: node.version,
            size_bytes,
        })),
        Err(textenc::NotText::Binary) => Err(ApiError::invalid_input(NOT_TEXT)),
        // 文字コードを決められない文章は、見るだけにする（保存すると中身が壊れるので、画面は編集を出さない）。
        Err(textenc::NotText::UnknownEncoding) => {
            if bytes.iter().take(BINARY_SNIFF_BYTES).any(|b| *b == 0) {
                return Err(ApiError::invalid_input(NOT_TEXT));
            }
            Ok(Json(TextFileResponse {
                text: String::from_utf8_lossy(&bytes).into_owned(),
                encoding: None,
                bom: false,
                version: node.version,
                size_bytes,
            }))
        }
    }
}

/// テキストを、元の文字コードと BOM のまま新しい版として保存する（editor）。
#[utoipa::path(
    put,
    path = "/files/{id}/text",
    params(("id" = Uuid, Path, description = "ファイル ID")),
    request_body = SaveTextFileRequest,
    responses(
        (status = 200, description = "保存した新しい版のファイル", body = NodeResponse),
        (status = 400, description = "元の文字コードで書けない文字がある・文字コードを決められない・大きすぎる（`invalid_input` と画面に出す文言）"),
        (status = 401, description = "未認証"),
        (status = 403, description = "認可されていない"),
        (status = 404, description = "ファイルが無い"),
        (status = 409, description = "開いた後にほかの保存が入った（`version_conflict`・`current_version` 付き）"),
    ),
    security(("session" = [])),
)]
pub async fn put_text(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<SaveTextFileRequest>,
) -> Result<Json<NodeResponse>, ApiError> {
    let meta = state
        .storage
        .get_metadata(&ctx, id, trace.as_deref())
        .await?;
    if meta.kind != NodeKind::File {
        return Err(ApiError::NotFound);
    }
    // 開いた後に新しい版があれば、読み直さずに止める（書き込みの条件でも比べる）。
    if meta.version != req.base_version {
        return Err(storage::StorageError::VersionConflict {
            current: meta.version,
        }
        .into());
    }
    // 元の文字コードは、編集の元にした版の中身から決め直す（画面が送った文字コードは信じない）。
    if meta.size_bytes.unwrap_or(0) > TEXT_FILE_MAX_BYTES {
        return Err(ApiError::invalid_input(TOO_LARGE));
    }
    let (_, original_bytes) = state
        .storage
        .read_file_internal(&ctx, id, trace.as_deref())
        .await?;
    let original = textenc::decode_text(&original_bytes).map_err(|why| match why {
        textenc::NotText::Binary => ApiError::invalid_input(NOT_TEXT),
        textenc::NotText::UnknownEncoding => ApiError::invalid_input(UNKNOWN_ENCODING_SAVE),
    })?;
    let bytes = textenc::encode_like(&req.text, original.encoding, original.bom).map_err(
        |textenc::Unmappable(c)| {
            ApiError::invalid_input(&format!(
                "「{c}」は、このファイルの文字コード（{}）では保存できません。別の文字に置き換えてください。",
                original.encoding.name()
            ))
        },
    )?;
    if bytes.len() > TEXT_SAVE_MAX_BYTES {
        return Err(ApiError::invalid_input(SAVE_TOO_LARGE));
    }
    // 種類は今のまま（文字コードの指定〔charset〕も元の文字コードのままなので変えない）。種類の分からないファイルは
    // テキストとして保存する（以前の画面と同じ）。
    let content_type = match meta.content_type.as_deref().map(str::trim) {
        Some(ct)
            if !ct.is_empty()
                && !ct
                    .to_ascii_lowercase()
                    .starts_with("application/octet-stream") =>
        {
            ct.to_string()
        }
        _ => "text/plain".to_string(),
    };
    let node = state
        .storage
        .update_file_content_from_base(
            &ctx,
            id,
            req.base_version,
            &bytes,
            &content_type,
            trace.as_deref(),
        )
        .await?;
    Ok(Json(node.into()))
}
