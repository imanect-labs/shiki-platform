//! gui の語彙（Single Source of Truth・Task 6.2）。
//!
//! ハンドラ種などを Rust enum で**閉じた集合**として定義し、`#[derive(TS)]` で TypeScript 型を
//! 生成する（codegen が正・手書きミラー禁止）。閉集合の外は表現できない（fail-closed）。
//! authz / workflow-engine の vocab と同型。

/// variant と serde/TS 名の対応を単一定義から生成する（as_str/parse の乖離を構造的に防ぐ）。
macro_rules! vocab_enum {
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
            /// serde/TS/DB で共通の文字列表現。
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
    };
}
pub(crate) use vocab_enum;

vocab_enum! {
    /// 明示登録のサーバ側ハンドラ束縛（Task 6.5 の②）。閉じた集合＝未知ハンドラは表現不可能。
    pub enum HandlerKind {
        /// 図の値を整形テキストとしてスレッドへ投稿する（会話の図の ask・plan・`-> chat` 専用）。
        ChatSubmit => "chat.submit",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_all_vocab() {
        for h in HandlerKind::ALL {
            assert_eq!(HandlerKind::parse(h.as_str()), Some(*h));
        }
        assert_eq!(HandlerKind::parse("exec"), None);
    }
}
