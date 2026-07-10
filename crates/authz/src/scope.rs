//! 能力スコープの単一ソース（Single Source of Truth・Task 9.1・design §4.1）。
//!
//! 公開 API ゲートウェイ（Task 9.6）がミニアプリへ貸せる能力を `<能力>.<操作>` の閉じた
//! 集合で定義する。ミニアプリのマニフェスト（`requested_scopes`）はこの語彙へ照合され、
//! 実在しないスコープ名（LLM/開発者由来のハルシネーション）は保存時に拒否される。
//!
//! ここで定義するのは**粗い語彙**（能力面の名前）だけであり、インスタンス単位の実認可は
//! 依然 OpenFGA（ReBAC）＋行レベル述語が担う（語彙の型安全 ≠ 認可判定）。Rust enum を正とし
//! `#[derive(TS)]` で TypeScript 型を生成する（手書きミラー禁止）。

/// variant と serde/TS 名の対応を単一定義から生成する（vocab の他 enum と同型）。
macro_rules! scope_enum {
    (
        $(#[$attr:meta])*
        $vis:vis enum $enum_name:ident {
            $( $(#[$vattr:meta])* $variant:ident => $name:literal, )+
        }
    ) => {
        $(#[$attr])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash,
            serde::Serialize, serde::Deserialize, ts_rs::TS,
        )]
        #[ts(export)]
        $vis enum $enum_name {
            $( $(#[$vattr])* #[serde(rename = $name)] $variant, )+
        }

        impl $enum_name {
            /// serde/TS/トークン scope で共通の文字列表現（`<能力>.<操作>`）。
            $vis const fn as_str(self) -> &'static str {
                match self { $( Self::$variant => $name, )+ }
            }

            /// 文字列から閉集合へ（未知は None・fail-closed）。
            $vis fn parse(s: &str) -> Option<Self> {
                match s { $( $name => Some(Self::$variant), )+ _ => None }
            }

            /// 全 variant（カタログ列挙・roundtrip テスト用）。
            $vis const ALL: &'static [$enum_name] = &[ $( Self::$variant, )+ ];
        }

        impl std::fmt::Display for $enum_name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

scope_enum! {
    /// ミニアプリへ貸せる能力スコープ（Task 9.1・公開 API ゲートウェイの能力面）。
    ///
    /// 各操作はリソース束縛＋per-call OpenFGA に従う（スコープは上限・実認可は ReBAC）。
    /// storage/data/rag/identity/events/notify/llm/agent の 8 能力を薄く再公開する。
    pub enum CapabilityScope {
        /// ファイル/フォルダの読取（StorageService 経由・個人 ReBAC）。
        StorageRead => "storage.read",
        /// ファイル/フォルダの書込。
        StorageWrite => "storage.write",
        /// 構造化データの読取（行レベル述語込み）。
        DataRead => "data.read",
        /// 構造化データの書込（サーバ検証込み）。
        DataWrite => "data.write",
        /// テーブルスキーマの参照/アップグレード同意時の additive 変更。
        DataSchema => "data.schema",
        /// permission-aware RAG 検索（個人 ReBAC 再チェック）。
        RagQuery => "rag.query",
        /// 呼出ユーザーの最小 identity（id/表示名/所属ロール）。
        IdentityRead => "identity.read",
        /// アプリ宛イベントの購読。
        EventsSubscribe => "events.subscribe",
        /// 通知の送信。
        NotifySend => "notify.send",
        /// raw LLM 呼び出し（アプリがプロンプト供給）。
        LlmInvoke => "llm.invoke",
        /// エージェント起動（ツール＋RAG・ユーザー ReBAC で絞る）。
        AgentInvoke => "agent.invoke",
    }
}

impl CapabilityScope {
    /// space 区切りの scope 文字列（Keycloak トークンの `scope` クレーム形式）をパースする。
    ///
    /// **fail-closed**: 1 つでも未知スコープがあれば `Err`（未知スコープを黙って無視しない）。
    pub fn parse_scope_string(raw: &str) -> Result<Vec<CapabilityScope>, String> {
        raw.split_whitespace()
            .filter(|s| !s.is_empty())
            // OIDC 標準スコープ（openid/profile/email）は能力スコープではないため読み飛ばす。
            .filter(|s| !matches!(*s, "openid" | "profile" | "email" | "offline_access"))
            .map(|s| Self::parse(s).ok_or_else(|| format!("未知の能力スコープ: {s}")))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_all_scopes() {
        for s in CapabilityScope::ALL {
            assert_eq!(CapabilityScope::parse(s.as_str()), Some(*s));
        }
        assert_eq!(CapabilityScope::parse("storage.delete"), None);
        assert_eq!(CapabilityScope::parse(""), None);
    }

    #[test]
    fn scope_string_fail_closed() {
        let ok = CapabilityScope::parse_scope_string("openid data.read rag.query").unwrap();
        assert_eq!(
            ok,
            vec![CapabilityScope::DataRead, CapabilityScope::RagQuery]
        );
        // 未知スコープが 1 つでもあれば全体を拒否する。
        assert!(CapabilityScope::parse_scope_string("data.read bogus.scope").is_err());
    }

    #[test]
    fn serde_uses_dotted_names() {
        assert_eq!(
            serde_json::to_string(&CapabilityScope::AgentInvoke).unwrap(),
            "\"agent.invoke\""
        );
        let s: CapabilityScope = serde_json::from_str("\"data.write\"").unwrap();
        assert_eq!(s, CapabilityScope::DataWrite);
    }
}
