//! JTD パースの資源上限（トラックJTD）。
//!
//! JTD は**ユーザーがアップロードした外部由来のバイナリ**であり、敵対的入力として扱う
//! （CLAUDE.md「サンドボックス由来の入力は敵対的として扱う」と同じ姿勢）。しかも docx/pdf と
//! 違ってパースは worker 往復ではなく **shiki-server のプロセス内**で走るため、資源枯渇は
//! そのまま API 全体の可用性に効く。したがって上流 `rjtd_core::ParseLimits` の既定
//! （入力 64 MiB・展開 256 MiB）をそのまま使わず、我々の運用値まで締め直す。

use rjtd_core::ParseLimits;

/// JTD パースに掛ける上限。
///
/// 既定値の根拠:
/// - `max_input_bytes` = 32 MiB — 実物の申請書・論文テンプレートは 60〜100 KB。一太郎の
///   画像入り文書でも数 MB に収まる。32 MiB は「正当な文書は必ず通る」側に十分な余裕を取りつつ、
///   1 リクエストが確保するバイト数を有界にする。
/// - `max_document_text_bytes` = 64 MiB — `.jtdc`（LHA 圧縮）経由の展開後サイズ。圧縮爆弾で
///   入力上限を迂回されないよう、展開側にも独立した天井を置く。
/// - `max_expansion_ratio` = 64 — 圧縮率そのものの上限。サイズ上限だけだと「上限すれすれまで
///   膨らむ小さな入力」を大量に投げる攻撃が通るため、比率でも切る。
// 3 フィールドすべてが `max_` 始まりなのは意図（すべて上限値）。struct_field_names の
// 助言に従って接頭辞を落とすと「何の値か」が読めなくなるため、ここは抑止する。
#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JtdLimits {
    max_input_bytes: usize,
    max_document_text_bytes: usize,
    max_expansion_ratio: usize,
}

const MIB: usize = 1024 * 1024;

impl JtdLimits {
    /// 運用既定値。
    pub const DEFAULT: Self = Self {
        max_input_bytes: 32 * MIB,
        max_document_text_bytes: 64 * MIB,
        max_expansion_ratio: 64,
    };

    /// 入力バイト数の上限。
    pub const fn max_input_bytes(self) -> usize {
        self.max_input_bytes
    }

    /// 入力バイト数の上限を差し替える（テストと、将来の設定注入のため）。
    #[must_use]
    pub const fn with_max_input_bytes(mut self, bytes: usize) -> Self {
        self.max_input_bytes = bytes;
        self
    }

    /// 展開後 `DocumentText` の上限を差し替える。
    #[must_use]
    pub const fn with_max_document_text_bytes(mut self, bytes: usize) -> Self {
        self.max_document_text_bytes = bytes;
        self
    }

    /// 圧縮展開率の上限を差し替える。
    #[must_use]
    pub const fn with_max_expansion_ratio(mut self, ratio: usize) -> Self {
        self.max_expansion_ratio = ratio;
        self
    }

    /// 上流 `rjtd_core` の上限型へ写す。
    ///
    /// 上流の型を公開 API に出さないのは、将来 rjtd を別実装へ差し替えても
    /// `crates/jtd` の公開型が変わらないようにするため（CLAUDE.md「差し替えはトレイト裏で」）。
    pub(crate) fn to_parse_limits(self) -> ParseLimits {
        ParseLimits::DEFAULT
            .with_max_input_bytes(self.max_input_bytes)
            .with_max_decompressed_bytes(self.max_document_text_bytes)
            .with_max_total_decompressed_bytes(self.max_document_text_bytes)
            .with_max_decompression_ratio(self.max_expansion_ratio)
    }
}

impl Default for JtdLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[cfg(test)]
mod tests {
    use super::{JtdLimits, MIB};

    #[test]
    fn defaults_are_stricter_than_upstream() {
        let ours = JtdLimits::DEFAULT.to_parse_limits();
        let upstream = rjtd_core::ParseLimits::DEFAULT;

        assert!(
            ours.max_input_bytes() < upstream.max_input_bytes(),
            "入力上限は上流既定（64 MiB）より締めていること"
        );
        assert!(
            ours.max_total_decompressed_bytes() <= upstream.max_total_decompressed_bytes(),
            "展開上限は上流既定を超えないこと"
        );
    }

    #[test]
    fn overrides_reach_upstream_limit_type() {
        let limits = JtdLimits::DEFAULT
            .with_max_input_bytes(123)
            .with_max_document_text_bytes(456);

        assert_eq!(limits.max_input_bytes(), 123);

        let parse_limits = limits.to_parse_limits();
        assert_eq!(parse_limits.max_input_bytes(), 123);
        assert_eq!(parse_limits.max_total_decompressed_bytes(), 456);
        assert!(
            parse_limits.check_input_size(124).is_err(),
            "入力上限を超えたら弾かれること"
        );
    }

    #[test]
    fn expansion_ratio_caps_compression_bombs() {
        // 展開後サイズだけでは「上限すれすれまで膨らむ小さな入力」を止められないため、
        // 比率でも切っていることを確かめる。上流の比率下限（1 MiB）を超える大きさで試す。
        let parse_limits = JtdLimits::DEFAULT
            .with_max_expansion_ratio(4)
            .to_parse_limits();

        assert!(
            parse_limits.check_lh5_output_size(2 * MIB, 8 * MIB).is_ok(),
            "4 倍ちょうどは通ること"
        );
        assert!(
            parse_limits
                .check_lh5_output_size(2 * MIB, 8 * MIB + 1)
                .is_err(),
            "4 倍を超えたら弾かれること"
        );
    }

    #[test]
    fn default_impl_matches_default_const() {
        assert_eq!(JtdLimits::default(), JtdLimits::DEFAULT);
    }
}
