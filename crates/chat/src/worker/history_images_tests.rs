#![allow(clippy::unwrap_used)]
use super::*;
use std::sync::Mutex;

fn ctx() -> AuthContext {
    AuthContext::new(
        authz::Principal {
            kind: authz::PrincipalKind::User,
            id: "alice".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some("images".into()),
        },
        "acme".into(),
        authz::TenantId::parse("images").unwrap(),
    )
}
fn message(role: Role, files: &[(&str, &str)]) -> Message {
    Message {
        id: Uuid::new_v4(),
        role,
        content: files
            .iter()
            .map(|(id, name)| ContentBlock::FileRef {
                node_id: (*id).into(),
                name: (*name).into(),
            })
            .collect(),
        agent_mode: false,
        parent_id: None,
        sibling_ids: vec![],
        invoked_actions: vec![],
        bound_actions: vec![],
        ui_runs: vec![],
        created_at: chrono::Utc::now(),
    }
}
struct Reader {
    calls: Mutex<Vec<String>>,
    bytes: Option<Vec<u8>>,
}
#[async_trait::async_trait]
impl AttachmentStore for Reader {
    async fn read(
        &self,
        ctx: &AuthContext,
        id: &str,
        max: u64,
        _: Option<&str>,
    ) -> Result<Vec<u8>, agent_core::ToolError> {
        assert_eq!(ctx.principal().id, "alice");
        assert_eq!(max, MAX_IMAGE_BYTES);
        self.calls.lock().unwrap().push(id.into());
        self.bytes
            .clone()
            .ok_or_else(|| agent_core::ToolError::Invalid("private detail".into()))
    }
}
fn reader(bytes: Option<Vec<u8>>) -> Reader {
    Reader {
        calls: Mutex::new(vec![]),
        bytes,
    }
}
fn png() -> Vec<u8> {
    let mut header = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
    header.extend_from_slice(&640_u32.to_be_bytes());
    header.extend_from_slice(&320_u32.to_be_bytes());
    header
}

#[tokio::test]
async fn newest_user_images_only_and_deduplicated() {
    let messages = vec![
        message(Role::User, &[("old", "old.png"), ("pdf", "a.pdf")]),
        message(Role::Assistant, &[("assistant", "a.png")]),
        message(
            Role::User,
            &[
                ("old", "reused.PNG"),
                ("b", "b.jpg"),
                ("c", "c.gif"),
                ("d", "d.webp"),
                ("e", "e.png"),
            ],
        ),
    ];
    let reader = reader(Some(png()));
    let result = load_images(&ctx(), &messages, Some(&reader)).await;
    assert_eq!(*reader.calls.lock().unwrap(), ["b", "c", "d", "e"]);
    assert!(result[&messages[2].id]
        .iter()
        .any(|b| matches!(b, Block::Text { text } if text.contains("枚数上限"))));
    assert_eq!(
        result
            .values()
            .flatten()
            .filter(|b| matches!(b, Block::Image { .. }))
            .count(),
        4
    );
    assert!(!result.contains_key(&messages[0].id));
    assert!(!result.contains_key(&messages[1].id));
}

#[test]
fn detects_only_supported_actual_bytes() {
    assert_eq!(media_type(&png()), Some("image/png"));
    assert_eq!(media_type(b"\xff\xd8\xffabc"), Some("image/jpeg"));
    assert_eq!(media_type(b"GIF87a"), Some("image/gif"));
    assert_eq!(media_type(b"GIF89a"), Some("image/gif"));
    assert_eq!(media_type(b"RIFF1234WEBP"), Some("image/webp"));
    assert_eq!(media_type(b"RIFF1234NOPE"), None);
    assert_eq!(media_type(b"<svg>"), None);
    assert_eq!(media_type(b""), None);
}

#[tokio::test]
async fn failure_unsupported_and_oversize_are_explicit() {
    let messages = vec![message(Role::User, &[("a", "a.svg")])];
    for bytes in [
        None,
        Some(b"<svg>".to_vec()),
        Some(vec![0; MAX_IMAGE_BYTES as usize + 1]),
    ] {
        let reader = reader(bytes);
        let result = load_images(&ctx(), &messages, Some(&reader)).await;
        assert!(!result
            .values()
            .flatten()
            .any(|b| matches!(b, Block::Image { .. })));
        let Block::Text { text } = &result[&messages[0].id][0] else {
            panic!("note")
        };
        assert!(text.contains("画像"));
        assert!(!text.contains("private detail"));
    }
    let result = load_images(&ctx(), &messages, None).await;
    assert!(
        matches!(&result[&messages[0].id][0], Block::Text { text } if text.contains("読み込めません"))
    );
    assert!(load_images(&ctx(), &[], None).await.is_empty());
}

#[test]
fn checkpoint_omits_bytes_and_refreshes_or_revokes_them() {
    let image = Block::Image {
        media_type: "image/png".into(),
        source_id: "a".into(),
        data: ImageData("PRIVATE_IMAGE_BYTES".into()),
    };
    assert!(!format!("{image:?}").contains("PRIVATE_IMAGE_BYTES"));
    let history = vec![llm_gateway::Message {
        role: llm_gateway::Role::User,
        content: vec![image.clone()],
    }];
    let checkpoint = agent_core::Checkpoint::start(history.clone());
    let saved = serde_json::to_string(&checkpoint).unwrap();
    assert!(!saved.contains("PRIVATE_IMAGE_BYTES"));
    let mut restored: agent_core::Checkpoint = serde_json::from_str(&saved).unwrap();
    refresh_checkpoint_images(&mut restored, &history);
    assert_eq!(restored.messages[0].content[0], image);
    refresh_checkpoint_images(&mut restored, &[]);
    assert!(
        matches!(&restored.messages[0].content[0], Block::Text { text } if text.contains("推測しない"))
    );
}

#[tokio::test]
async fn preserves_attachment_order_while_loading_in_parallel() {
    struct ParallelReader(tokio::sync::Barrier);
    #[async_trait::async_trait]
    impl AttachmentStore for ParallelReader {
        async fn read(
            &self,
            _: &AuthContext,
            _: &str,
            _: u64,
            _: Option<&str>,
        ) -> Result<Vec<u8>, agent_core::ToolError> {
            self.0.wait().await;
            Ok(png())
        }
    }
    let messages = vec![message(
        Role::User,
        &[("first", "a.png"), ("second", "b.png")],
    )];
    let reader = ParallelReader(tokio::sync::Barrier::new(2));
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        load_images(&ctx(), &messages, Some(&reader)),
    )
    .await
    .unwrap();
    let ids: Vec<_> = result[&messages[0].id]
        .iter()
        .filter_map(|b| match b {
            Block::Image { source_id, .. } => Some(source_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(ids, ["first", "second"]);
}

#[tokio::test]
async fn oversized_or_unreadable_dimensions_are_not_sent() {
    let messages = vec![message(Role::User, &[("a", "a.png")])];
    for (width, height, allowed) in [
        (8000_u32, 1_u32, true),
        (8001, 1, false),
        (1, 9000, false),
        (0, 1, false),
    ] {
        let mut bytes = png();
        bytes[16..20].copy_from_slice(&width.to_be_bytes());
        bytes[20..24].copy_from_slice(&height.to_be_bytes());
        let store = reader(Some(bytes));
        let result = load_images(&ctx(), &messages, Some(&store)).await;
        assert_eq!(
            result
                .values()
                .flatten()
                .any(|b| matches!(b, Block::Image { .. })),
            allowed
        );
    }
    let store = reader(Some(b"\x89PNG\r\n\x1a\n".to_vec()));
    let result = load_images(&ctx(), &messages, Some(&store)).await;
    assert!(matches!(&result[&messages[0].id][0], Block::Text { text } if text.contains("寸法")));
}
