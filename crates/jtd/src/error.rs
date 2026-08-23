//! `crates/jtd` のエラー型。
//!
//! 上流 `rjtd_core::Error` をそのまま公開しない。将来 rjtd を別実装へ差し替えても
//! 呼び出し側が変わらないようにするため（CLAUDE.md「差し替えはトレイト裏で」）。
//! また、外部由来バイナリの解析失敗理由をそのままユーザーへ返すとフォーマット解析の
//! オラクルになるため、公開メッセージは粗い粒度に留め、詳細は `tracing` へ落とす。

/// JTD の読み取りに失敗した理由。
#[derive(Debug, thiserror::Error)]
pub enum JtdError {
    /// CFB ですらない、あるいは JTD として認識できない。
    #[error("一太郎ファイルとして認識できません")]
    NotJtd,
    /// CFB ではあるが、本文（`DocumentText`）に到達できない。
    /// 一太郎 2004（v14）以降の新形式もここに落ちる（現時点ではスコープ外）。
    #[error("対応していない一太郎形式です")]
    Unsupported,
    /// 構造が壊れている（FAT の不整合・レコード長の詐称など）。
    #[error("一太郎ファイルの構造が壊れています")]
    Malformed,
    /// 資源上限に触れた（巨大入力・圧縮爆弾）。
    #[error("一太郎ファイルが大きすぎます（{resource}: {actual} > {limit}）")]
    TooLarge {
        /// 触れた上限の名前（`input bytes` 等・上流の語彙をそのまま載せる）。
        resource: &'static str,
        /// 上限値。
        limit: usize,
        /// 実際の値。
        actual: usize,
    },
}

impl JtdError {
    /// 上流エラーを我々の語彙へ写す。理由の詳細は呼び出し側で `tracing` へ落とす。
    pub(crate) fn from_upstream(error: &rjtd_core::Error) -> Self {
        match error {
            rjtd_core::Error::NotFound(_) | rjtd_core::Error::Unsupported(_) => {
                JtdError::Unsupported
            }
            rjtd_core::Error::InvalidData(_) | rjtd_core::Error::Io(_) => JtdError::Malformed,
            rjtd_core::Error::ResourceLimit {
                resource,
                limit,
                actual,
            } => JtdError::TooLarge {
                resource,
                limit: *limit,
                actual: *actual,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::JtdError;

    #[test]
    fn upstream_errors_map_to_our_vocabulary() {
        assert!(matches!(
            JtdError::from_upstream(&rjtd_core::Error::NotFound("/DocumentText".into())),
            JtdError::Unsupported
        ));
        assert!(matches!(
            JtdError::from_upstream(&rjtd_core::Error::Unsupported("jtdc")),
            JtdError::Unsupported
        ));
        assert!(matches!(
            JtdError::from_upstream(&rjtd_core::Error::InvalidData("bad fat".into())),
            JtdError::Malformed
        ));
        assert!(matches!(
            JtdError::from_upstream(&rjtd_core::Error::Io("eof".into())),
            JtdError::Malformed
        ));
        assert!(matches!(
            JtdError::from_upstream(&rjtd_core::Error::ResourceLimit {
                resource: "input bytes",
                limit: 1,
                actual: 2,
            }),
            JtdError::TooLarge {
                resource: "input bytes",
                limit: 1,
                actual: 2
            }
        ));
    }

    #[test]
    fn messages_do_not_leak_upstream_detail() {
        // 解析失敗の内訳はフォーマット解析のオラクルになるため、公開メッセージには載せない。
        let message = JtdError::from_upstream(&rjtd_core::Error::InvalidData(
            "sector 42 points outside the FAT".into(),
        ))
        .to_string();

        assert!(!message.contains("sector"), "上流の詳細が漏れていないこと");
        assert_eq!(message, "一太郎ファイルの構造が壊れています");
    }

    #[test]
    fn too_large_reports_the_limit_it_hit() {
        let message = JtdError::TooLarge {
            resource: "input bytes",
            limit: 10,
            actual: 11,
        }
        .to_string();

        assert!(message.contains("input bytes"));
        assert!(message.contains("11"));
        assert!(message.contains("10"));
    }

    #[test]
    fn not_jtd_has_its_own_message() {
        assert_eq!(
            JtdError::NotJtd.to_string(),
            "一太郎ファイルとして認識できません"
        );
        assert_eq!(
            JtdError::Unsupported.to_string(),
            "対応していない一太郎形式です"
        );
    }
}
