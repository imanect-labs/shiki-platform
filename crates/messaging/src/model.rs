//! messaging の型（API DTO 兼ドメイン）。
//!
//! 本文は §4.4 の content blocks（[`chat::model::ContentBlock`]）を共有し、使えるのは
//! `text` / `file_ref` / `mention` の部分集合だけ（[`validate_blocks`]）。

use chat::model::{ContentBlock, Mention};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::Error;

/// 1 発言の本文の上限（文字数・design §4.14 本文）。
pub const MAX_TEXT_CHARS: usize = 4_000;
/// 1 発言の添付の上限。
pub const MAX_ATTACHMENTS: usize = 10;
/// 1 発言のブロック数の上限（テキストの分割・メンションを含む）。
pub const MAX_BLOCKS: usize = 200;
/// パーソナルエージェントの既定の名前。
pub const DEFAULT_AGENT_NAME: &str = "シキ";

/// チャンネルの種別。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChannelKind {
    Public,
    Private,
    /// 1 対 1 の DM（利用者と自分のパーソナルエージェントの DM を含む）。
    Dm,
}

impl ChannelKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            ChannelKind::Public => "public",
            ChannelKind::Private => "private",
            ChannelKind::Dm => "dm",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "public" => Some(ChannelKind::Public),
            "private" => Some(ChannelKind::Private),
            "dm" => Some(ChannelKind::Dm),
            _ => None,
        }
    }

    /// 参加前の発言を読めない種別か（非公開と DM・design §4.14 認可）。
    pub const fn hides_history_before_join(self) -> bool {
        !matches!(self, ChannelKind::Public)
    }
}

/// 通知の受け方（チャンネルごと）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum NotifyLevel {
    /// すべての発言を未読として数え、メンションを通知する。
    #[default]
    All,
    /// メンションだけ通知する。
    Mentions,
    /// 通知しない（ミュート）。
    None,
}

impl NotifyLevel {
    pub const fn as_str(self) -> &'static str {
        match self {
            NotifyLevel::All => "all",
            NotifyLevel::Mentions => "mentions",
            NotifyLevel::None => "none",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "mentions" => NotifyLevel::Mentions,
            "none" => NotifyLevel::None,
            _ => NotifyLevel::All,
        }
    }
}

/// 自分から見たチャンネル（一覧・詳細）。
#[allow(clippy::struct_excessive_bools)] // 画面に出す状態フラグの DTO（状態機械ではない）。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ChannelView {
    pub id: Uuid,
    pub kind: ChannelKind,
    /// DM は相手の表示名（自分のエージェントとの DM はエージェント名）。
    pub name: String,
    pub topic: String,
    pub announcement: bool,
    pub archived: bool,
    /// 自分が参加しているか（公開チャンネルは参加せずに読める）。
    pub joined: bool,
    /// 自分がチャンネルの admin か。
    pub is_admin: bool,
    /// 自分が投稿できるか。
    pub can_post: bool,
    /// DM の相手（自分のエージェントとの DM は `None`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dm_peer: Option<String>,
    /// 自分のパーソナルエージェントとの DM か（承認の通知が届く）。
    pub agent_dm: bool,
    pub member_count: i64,
    pub unread: i64,
    pub mentions: i64,
    pub notify: NotifyLevel,
    pub last_message_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// チャンネルの参加者。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MemberView {
    pub user_id: String,
    pub display_name: String,
    pub is_admin: bool,
    /// アナウンス専用チャンネルで投稿できる人か（通常のチャンネルでは全員 true）。
    pub can_post: bool,
    /// 組織から外れた（無効化された）利用者か。
    pub active: bool,
    pub joined_at: DateTime<Utc>,
}

/// 作成の入力。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateChannel {
    pub name: String,
    #[serde(default)]
    pub topic: String,
    /// `public` / `private`（DM は [`crate::MessagingService::open_dm`]）。
    pub kind: ChannelKind,
    #[serde(default)]
    pub announcement: bool,
    /// 作成と同時に招待する利用者。
    #[serde(default)]
    pub members: Vec<String>,
}

/// 変更の入力（admin だけ）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct UpdateChannel {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub topic: Option<String>,
}

/// 発言の書き手。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuthorKind {
    User,
    /// パーソナルエージェント（`author_id` は持ち主＝呼んだ人）。
    Assistant,
}

impl AuthorKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            AuthorKind::User => "user",
            AuthorKind::Assistant => "assistant",
        }
    }

    pub fn parse(s: &str) -> Self {
        if s == "assistant" {
            AuthorKind::Assistant
        } else {
            AuthorKind::User
        }
    }
}

/// 発言の書き手の表示。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AuthorView {
    pub kind: AuthorKind,
    /// 利用者 id（エージェントの発言では持ち主）。
    pub user_id: String,
    /// 表示名（エージェントの発言ではエージェント名）。
    pub display_name: String,
    /// 持ち主の表示名（エージェントの発言だけ。「〇〇 のエージェント」と必ず出す）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_name: Option<String>,
    /// エージェントのアイコンが設定済みか（`/messaging/agents/{user_id}/icon` で取る）。
    pub has_icon: bool,
    /// 組織から外れた（無効化された）利用者か（表示名に「（無効）」を付ける）。
    pub active: bool,
}

/// 添付（file_ref）の、閲覧者から見た状態。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FileView {
    /// `blocks` の何番目の file_ref か。
    pub block_index: usize,
    /// 閲覧者が読めるか。読めないときは node_id・名前・サイズを返さない。
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
}

/// リアクションの集計。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ReactionView {
    pub emoji: String,
    pub count: i64,
    /// 自分が付けたか。
    pub me: bool,
}

/// リンクのプレビュー（取得できる構成でだけ埋まる）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LinkPreview {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site_name: Option<String>,
}

/// 発言の状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MessageState {
    Normal,
    /// 「この発言は削除されました」。
    Deleted,
    /// 「この発言は保存期間を過ぎました」。
    Expired,
}

/// 閲覧者から見た 1 発言（通常の読取経路の唯一の出力型）。
///
/// 削除・期限切れの発言は `blocks` が空。編集前の本文はここには決して載らない。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MessageView {
    pub id: Uuid,
    pub channel_id: Uuid,
    pub parent_id: Option<Uuid>,
    pub author: AuthorView,
    pub blocks: Vec<ContentBlock>,
    pub files: Vec<FileView>,
    pub reactions: Vec<ReactionView>,
    pub link_previews: Vec<LinkPreview>,
    pub reply_count: i64,
    pub last_reply_at: Option<DateTime<Utc>>,
    pub state: MessageState,
    /// 「（編集済み）」の表示用（編集履歴は見せない）。
    pub edited: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// 投稿の入力。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NewMessage {
    #[serde(default)]
    pub parent_id: Option<Uuid>,
    pub blocks: Vec<ContentBlock>,
    /// パーソナルエージェントを呼んだとき、回答を自分だけに表示するか
    /// （自分の権限で答えさせる・design §4.14）。既定は公開返信。
    #[serde(default)]
    pub agent_private: bool,
}

/// 発言の検索結果。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SearchHit {
    pub channel_id: Uuid,
    pub channel_name: String,
    pub channel_kind: ChannelKind,
    pub message: MessageView,
}

/// メンションの通知（アプリ内通知）。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MentionNotice {
    pub message_id: Uuid,
    pub channel_id: Uuid,
    pub channel_name: String,
    pub parent_id: Option<Uuid>,
    pub author: AuthorView,
    /// 本文の冒頭（閲覧者が読めない発言は通知自体を返さない）。
    pub excerpt: String,
    pub via: String,
    pub read: bool,
    pub created_at: DateTime<Utc>,
}

/// 発言を [`MAX_TEXT_CHARS`] 等の上限と、使えるブロックの部分集合で検証する。
///
/// # Errors
///
/// 空・上限超過・使えないブロックの種類のとき [`Error::Invalid`]。
pub fn validate_blocks(blocks: &[ContentBlock]) -> Result<(), Error> {
    if blocks.is_empty() || blocks.len() > MAX_BLOCKS {
        return Err(Error::Invalid("本文が空か、長すぎます".into()));
    }
    let mut chars = 0usize;
    let mut files = 0usize;
    let mut meaningful = false;
    for b in blocks {
        match b {
            ContentBlock::Text { text } => {
                chars += text.chars().count();
                meaningful |= !text.trim().is_empty();
            }
            ContentBlock::FileRef { node_id, .. } => {
                if Uuid::parse_str(node_id).is_err() {
                    return Err(Error::Invalid("添付の指定が不正です".into()));
                }
                files += 1;
                meaningful = true;
            }
            ContentBlock::Mention { mention } => {
                meaningful = true;
                match mention {
                    Mention::User { user_id } if user_id.is_empty() => {
                        return Err(Error::Invalid("メンションの宛先が空です".into()))
                    }
                    Mention::Role { role_id } if role_id.is_empty() => {
                        return Err(Error::Invalid("メンションの宛先が空です".into()))
                    }
                    _ => {}
                }
            }
            _ => return Err(Error::Invalid("この種類の本文は送れません".into())),
        }
    }
    if !meaningful {
        return Err(Error::Invalid("本文が空です".into()));
    }
    if chars > MAX_TEXT_CHARS {
        return Err(Error::Invalid(format!(
            "本文は {MAX_TEXT_CHARS} 文字までです"
        )));
    }
    if files > MAX_ATTACHMENTS {
        return Err(Error::Invalid(format!(
            "添付は {MAX_ATTACHMENTS} 件までです"
        )));
    }
    Ok(())
}

/// 保存する形に正規化する（file_ref の名前を捨てる・本文を複製しない）。
#[must_use]
pub fn normalize_blocks(blocks: Vec<ContentBlock>) -> Vec<ContentBlock> {
    blocks
        .into_iter()
        .map(|b| match b {
            ContentBlock::FileRef { node_id, .. } => ContentBlock::FileRef {
                node_id: node_id.to_ascii_lowercase(),
                name: String::new(),
            },
            other => other,
        })
        .collect()
}

/// 検索・通知の抜粋に使う平文（テキストとメンションの表記だけ）。
#[must_use]
pub fn plain_text(blocks: &[ContentBlock]) -> String {
    let mut out = String::new();
    for b in blocks {
        match b {
            ContentBlock::Text { text } => out.push_str(text),
            ContentBlock::Mention {
                mention: Mention::Channel,
            } => out.push_str("@channel"),
            ContentBlock::Mention { .. } => out.push('@'),
            _ => {}
        }
    }
    out
}

/// 平文に、メンションを名前つきで書き出す（通知の抜粋・AI の文脈）。`users` は利用者 id → 表示名。
#[must_use]
pub fn plain_text_named<S: std::hash::BuildHasher>(
    blocks: &[ContentBlock],
    users: &std::collections::HashMap<String, String, S>,
) -> String {
    let mut out = String::new();
    for b in blocks {
        match b {
            ContentBlock::Text { text } => out.push_str(text),
            ContentBlock::Mention { mention } => match mention {
                Mention::User { user_id } => {
                    out.push('@');
                    out.push_str(users.get(user_id).map_or("利用者", String::as_str));
                }
                Mention::Role { role_id } => {
                    out.push('@');
                    out.push_str(role_id);
                }
                Mention::Channel => out.push_str("@channel"),
                Mention::Agent => out.push_str("@エージェント"),
            },
            _ => {}
        }
    }
    out
}

/// 本文のユーザーメンションの宛先 id。
#[must_use]
pub fn mentioned_users(blocks: &[ContentBlock]) -> Vec<String> {
    blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Mention {
                mention: Mention::User { user_id },
            } => Some(user_id.clone()),
            _ => None,
        })
        .collect()
}

/// 本文の中の URL（リンクのプレビューの候補・先頭から最大 `max` 件・重複なし）。
#[must_use]
pub fn extract_urls(blocks: &[ContentBlock], max: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for b in blocks {
        let ContentBlock::Text { text } = b else {
            continue;
        };
        for word in text.split(|c: char| c.is_whitespace() || "<>()\"'「」".contains(c)) {
            if !(word.starts_with("https://") || word.starts_with("http://")) {
                continue;
            }
            let url = word.trim_end_matches(['.', ',', '、', '。', ')', ']']);
            if url.len() > 2048 || out.iter().any(|u| u == url) {
                continue;
            }
            out.push(url.to_string());
            if out.len() >= max {
                return out;
            }
        }
    }
    out
}

/// 本文が自分のエージェントを呼んでいるか。
#[must_use]
pub fn mentions_agent(blocks: &[ContentBlock]) -> bool {
    blocks.iter().any(|b| {
        matches!(
            b,
            ContentBlock::Mention {
                mention: Mention::Agent
            }
        )
    })
}

/// 本文の file_ref の node_id（出現順）。
#[must_use]
pub fn file_ids(blocks: &[ContentBlock]) -> Vec<(usize, Uuid)> {
    blocks
        .iter()
        .enumerate()
        .filter_map(|(i, b)| match b {
            ContentBlock::FileRef { node_id, .. } => Uuid::parse_str(node_id).ok().map(|u| (i, u)),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> ContentBlock {
        ContentBlock::Text { text: s.into() }
    }

    #[test]
    fn rejects_blocks_outside_the_subset() {
        let err = validate_blocks(&[ContentBlock::Thinking { text: "x".into() }]);
        assert!(matches!(err, Err(Error::Invalid(_))));
        assert!(validate_blocks(&[]).is_err());
        assert!(validate_blocks(&[text("   ")]).is_err());
        assert!(validate_blocks(&[text("hi")]).is_ok());
    }

    #[test]
    fn enforces_text_and_attachment_limits() {
        assert!(validate_blocks(&[text(&"あ".repeat(MAX_TEXT_CHARS))]).is_ok());
        assert!(validate_blocks(&[text(&"あ".repeat(MAX_TEXT_CHARS + 1))]).is_err());
        let file = || ContentBlock::FileRef {
            node_id: Uuid::nil().to_string(),
            name: String::new(),
        };
        let ten: Vec<_> = (0..MAX_ATTACHMENTS).map(|_| file()).collect();
        assert!(validate_blocks(&ten).is_ok());
        let eleven: Vec<_> = (0..=MAX_ATTACHMENTS).map(|_| file()).collect();
        assert!(validate_blocks(&eleven).is_err());
        assert!(validate_blocks(&[ContentBlock::FileRef {
            node_id: "../etc".into(),
            name: String::new()
        }])
        .is_err());
    }

    #[test]
    fn normalize_drops_file_names() {
        let id = Uuid::new_v4().to_string().to_uppercase();
        let out = normalize_blocks(vec![ContentBlock::FileRef {
            node_id: id.clone(),
            name: "給与.xlsx".into(),
        }]);
        assert_eq!(
            out,
            vec![ContentBlock::FileRef {
                node_id: id.to_ascii_lowercase(),
                name: String::new()
            }]
        );
    }

    #[test]
    fn extracts_urls_without_duplicates() {
        let urls = extract_urls(
            &[text(
                "見て https://example.com/a。 と (https://example.com/a) と http://x.test/b",
            )],
            3,
        );
        assert_eq!(urls, vec!["https://example.com/a", "http://x.test/b"]);
    }

    #[test]
    fn detects_agent_mention_and_plain_text() {
        let blocks = vec![
            ContentBlock::Mention {
                mention: Mention::Agent,
            },
            text(" 要約して"),
        ];
        assert!(mentions_agent(&blocks));
        assert_eq!(plain_text(&blocks), "@ 要約して");
        assert!(!mentions_agent(&[text("@シキ")]));
    }
}
