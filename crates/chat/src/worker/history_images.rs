//! 認可済みの添付画像をLLMの中立ブロックへ写す。履歴の新しい画像4枚、各4MiBまで。
use std::collections::{HashMap, HashSet};

use agent_core::AttachmentStore;
use authz::AuthContext;
use base64::Engine;
use llm_gateway::{Block, ImageData};
use uuid::Uuid;

use crate::model::{ContentBlock, Message, Role};

#[cfg(test)]
#[path = "history_images_tests.rs"]
mod tests;

const MAX_IMAGES: usize = 4;
const MAX_IMAGE_BYTES: u64 = 4 * 1024 * 1024;

fn image_name(name: &str) -> bool {
    name.rsplit('.').next().is_some_and(|ext| {
        matches!(
            ext.to_ascii_lowercase().as_str(),
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "avif" | "svg" | "bmp"
        )
    })
}

/// 形式は利用者のファイル名やContent-Typeではなく実バイトで決める。
fn media_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else {
        None
    }
}

// デコードやピクセル配列の確保をせず、プロバイダ共通の安全な上限で弾く。
fn valid_dimensions(bytes: &[u8]) -> bool {
    imagesize::blob_size(bytes)
        .is_ok_and(|size| (1..=8000).contains(&size.width) && (1..=8000).contains(&size.height))
}

pub(super) async fn load_images(
    ctx: &AuthContext,
    messages: &[Message],
    store: Option<&dyn AttachmentStore>,
) -> HashMap<Uuid, Vec<Block>> {
    let mut out = HashMap::<Uuid, Vec<Block>>::new();
    let mut seen = HashSet::new();
    let mut selected = Vec::new();
    for message in messages.iter().rev().filter(|m| m.role == Role::User) {
        for block in message.content.iter().rev() {
            let ContentBlock::FileRef { node_id, name } = block else {
                continue;
            };
            if !image_name(name) || !seen.insert(node_id.clone()) {
                continue;
            }
            if selected.len() >= MAX_IMAGES {
                out.entry(message.id).or_default().push(Block::Text { text: format!("[画像添付「{name}」は枚数上限のため画像本体を送信していません。新しい{MAX_IMAGES}枚が対象です。]") });
            } else {
                selected.push((message.id, node_id.as_str(), name.as_str()));
            }
        }
    }
    selected.reverse(); // 選択は新しい順、送信は投稿内の添付順を保つ。
                        // 最大4件に絞ってから並行読込する。件数・同時実行数・合計メモリを有界に保つ。
    let results =
        futures::future::join_all(selected.into_iter().map(|(id, node_id, name)| async move {
            (id, load_one(ctx, store, node_id, name).await)
        }))
        .await;
    for (id, blocks) in results {
        out.entry(id).or_default().extend(blocks);
    }
    out
}

async fn load_one(
    ctx: &AuthContext,
    store: Option<&dyn AttachmentStore>,
    node_id: &str,
    name: &str,
) -> Vec<Block> {
    let result = match store {
        Some(store) => store.read(ctx, node_id, MAX_IMAGE_BYTES, None).await,
        None => Err(agent_core::ToolError::Unavailable(
            "画像読込が未構成です".into(),
        )),
    };
    let note = match result {
        Ok(bytes) if bytes.len() as u64 <= MAX_IMAGE_BYTES => match media_type(&bytes) {
            Some(media_type) if valid_dimensions(&bytes) => return vec![Block::Text { text: format!("[画像添付: {name}]") }, Block::Image { media_type: media_type.into(), source_id: node_id.into(), data: ImageData(base64::engine::general_purpose::STANDARD.encode(bytes).into()) }],
            Some(_) => format!("[画像添付「{name}」は寸法を確認できないか縦横8,000pxの上限を超えるため画像本体を送信していません。]"),
            None => format!("[画像添付「{name}」は画像本体を送信できません。PNG/JPEG/GIF/WebP形式を使用してください。]"),
        },
        Ok(_) => format!("[画像添付「{name}」は4MiBのサイズ上限を超えるため画像本体を送信していません。]"),
        Err(_) => format!("[画像添付「{name}」を読み込めません。アクセス権・サイズ上限・ファイルの存在を確認してください。画像内容は推測しないでください。]"),
    };
    vec![Block::Text { text: note }]
}

/// 保存時に省いた実体を現在の権限で復元し、読めなくなった画像は再送しない。
pub(super) fn refresh_checkpoint_images(
    checkpoint: &mut agent_core::Checkpoint,
    history: &[llm_gateway::Message],
) {
    let images: HashMap<_, _> = history
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|b| {
            if let Block::Image { source_id, .. } = b {
                Some((source_id.as_str(), b))
            } else {
                None
            }
        })
        .collect();
    for block in checkpoint.messages.iter_mut().flat_map(|m| &mut m.content) {
        if let Block::Image { source_id, .. } = block {
            *block = images.get(source_id.as_str()).map_or_else(
                || Block::Text {
                    text: "[以前の画像添付を再読込できません。画像内容は推測しないでください。]"
                        .into(),
                },
                |image| (*image).clone(),
            );
        }
    }
}
