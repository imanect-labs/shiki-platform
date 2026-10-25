//! 検索・本文の読み出しの監査に残す「どの要求の・どの回答のためか」（#829）。
//!
//! trace_id は要求ごとの相関 ID。チャットからの検索では、それに加えて会話（thread）と、
//! いま生成している回答（assistant のメッセージ）を残し、1 回答に紐づく権限判定と引用を
//! 監査ログから一覧できるようにする（Task 8.1/8.2）。

use uuid::Uuid;

/// 監査に添える文脈。`Option<&str>`（trace_id だけ）からも作れる。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AuditTrail<'a> {
    pub trace_id: Option<&'a str>,
    /// 会話のスレッド。
    pub thread_id: Option<Uuid>,
    /// 生成中の回答（assistant のメッセージ）。
    pub message_id: Option<Uuid>,
}

impl<'a> AuditTrail<'a> {
    /// 会話の回答のための検索・読み出し。
    #[must_use]
    pub fn for_answer(trace_id: Option<&'a str>, thread_id: Uuid, message_id: Uuid) -> Self {
        AuditTrail {
            trace_id,
            thread_id: Some(thread_id),
            message_id: Some(message_id),
        }
    }

    /// 監査の metadata に会話と回答を書き足す（無ければ何もしない）。
    pub fn annotate(&self, metadata: &mut serde_json::Value) {
        if let Some(obj) = metadata.as_object_mut() {
            if let Some(t) = self.thread_id {
                obj.insert("thread_id".into(), serde_json::json!(t));
            }
            if let Some(m) = self.message_id {
                obj.insert("message_id".into(), serde_json::json!(m));
            }
        }
    }
}

impl<'a> From<Option<&'a str>> for AuditTrail<'a> {
    fn from(trace_id: Option<&'a str>) -> Self {
        AuditTrail {
            trace_id,
            ..AuditTrail::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annotate_adds_only_known_ids() {
        let mut m = serde_json::json!({"k": 1});
        AuditTrail::from(Some("t")).annotate(&mut m);
        assert_eq!(m, serde_json::json!({"k": 1}));
        let (th, msg) = (Uuid::nil(), Uuid::max());
        let trail = AuditTrail::for_answer(Some("t"), th, msg);
        trail.annotate(&mut m);
        assert_eq!(m["thread_id"], serde_json::json!(th));
        assert_eq!(m["message_id"], serde_json::json!(msg));
        assert_eq!(trail.trace_id, Some("t"));
    }
}
