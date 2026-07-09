//! generative UI の語彙（Single Source of Truth・Task 6.2）。
//!
//! 信頼コンポーネント・カタログ／ハンドラ種／チャート種を Rust enum で**閉じた集合**として
//! 定義し、`#[derive(TS)]` で TypeScript 型を生成する（codegen が正・手書きミラー禁止）。
//! カタログは design-caveats の指摘どおり**信頼境界**であり、この閉集合の外は
//! [`validate`](crate::validate) が保存・描画の前段で拒否する。
//!
//! authz / workflow-engine の vocab と同型。将来コンポーネントは variant を先行予約して
//! serde 名を凍結し（後方互換の固定）、`available()` が false の間は検証で拒否する。

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

vocab_enum! {
    /// 信頼コンポーネント・カタログ（Task 6.2）。
    ///
    /// [`UiNode`](crate::spec::UiNode) の serde タグ（`component`）と 1:1 対応する
    /// （対応は `UiNode::kind()` に単一化し、drift はテストで固定）。
    /// `text_input` / `select` はフォーム部品としてのみ出現する（[`FormField`](crate::spec::FormField)）。
    pub enum ComponentKind {
        Container => "container",
        Text => "text",
        Link => "link",
        Form => "form",
        TextInput => "text_input",
        Select => "select",
        Button => "button",
        Table => "table",
        Chart => "chart",
        // ---- 将来予約（serde 名を凍結・Phase 6 では検証が拒否する） ----
        /// 地図（タイル表示＋ピン・design §4.7。外部タイル依存のため後続）。
        Map => "map",
        /// 画像（ストレージ node 参照のみ許す設計を確定してから有効化する）。
        Image => "image",
    }
}

impl ComponentKind {
    /// Phase 6 で描画・保存を許すカタログ部分集合（予約 variant は false）。
    pub fn available(self) -> bool {
        !matches!(self, ComponentKind::Map | ComponentKind::Image)
    }
}

vocab_enum! {
    /// 明示登録のサーバ側ハンドラ束縛（Task 6.5 の②）。閉じた集合＝未知ハンドラは表現不可能。
    pub enum HandlerKind {
        /// フォーム値を整形テキストとしてスレッドへ投稿する（チャット UI 専用）。
        ChatSubmit => "chat.submit",
    }
}

vocab_enum! {
    /// チャート種（vega-lite 的サブセット・design §4.7）。
    pub enum ChartKind {
        Bar => "bar",
        Line => "line",
        Area => "area",
        Pie => "pie",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_all_vocab() {
        for c in ComponentKind::ALL {
            assert_eq!(ComponentKind::parse(c.as_str()), Some(*c));
        }
        for h in HandlerKind::ALL {
            assert_eq!(HandlerKind::parse(h.as_str()), Some(*h));
        }
        for k in ChartKind::ALL {
            assert_eq!(ChartKind::parse(k.as_str()), Some(*k));
        }
        assert_eq!(ComponentKind::parse("iframe"), None);
        assert_eq!(HandlerKind::parse("exec"), None);
    }

    #[test]
    fn reserved_components_are_unavailable() {
        assert!(!ComponentKind::Map.available());
        assert!(!ComponentKind::Image.available());
        assert!(ComponentKind::Form.available());
        assert!(ComponentKind::Chart.available());
    }
}
