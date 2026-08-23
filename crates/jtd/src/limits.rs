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
/// **上限は入力サイズで掛ける。** パーサが作る中間表現は入力に対して増幅するため
/// （実測: `0x001D` を敷き詰めた `DocumentText` で **入力の約 40 倍**・31 MiB → 1.3 GiB / 37 秒）、
/// 「出力を測って止める」形にはできない。よって入力側を実物の分布に合わせて締める。
///
/// 既定値の根拠:
/// - `max_input_bytes` = 8 MiB — 実物の申請書・論文テンプレートは 60〜100 KB。8 MiB は
///   最大サンプルの約 80 倍で、画像入りの実文書にも十分な余裕がある。上の増幅率を掛けても
///   1 パースが確保するバイト数が数百 MB に収まる大きさとして選んだ。
/// - `max_document_text_bytes` = 8 MiB — `.jtdc`（LHA 圧縮）経由の展開後サイズ。
///   **入力上限より大きくしてはいけない**（大きいと、圧縮を経由するだけで入力上限を
///   超える CFB を再パースさせられ、締めた意味が消える）。
/// - `max_expansion_ratio` = 64 — 圧縮率そのものの上限。サイズ上限だけだと「上限すれすれまで
///   膨らむ小さな入力」を大量に投げる攻撃が通るため、比率でも切る。
///
/// **これは 1 パースあたりの上限でしかない。** 同時実行数の制限（semaphore）と
/// `spawn_blocking` への退避は、呼び出し側を配線するときに必ず入れること。
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

/// 展開率の下限バイト数。これ以下の出力には `max_expansion_ratio` を適用しない。
///
/// 小さな正当ファイルを比率だけで弾かないための逃がしだが、上流既定の 1 MiB は
/// 「1 MiB 未満なら圧縮率無制限」という穴になる。実物の `DocumentText` は 13〜74 KB なので
/// 64 KiB まで下げても正当な文書は通り、穴は 1/16 になる。
const EXPANSION_RATIO_FLOOR_BYTES: usize = 64 * 1024;

impl JtdLimits {
    /// 運用既定値。
    pub const DEFAULT: Self = Self {
        max_input_bytes: 8 * MIB,
        max_document_text_bytes: 8 * MIB,
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
        // 展開上限は入力上限で頭打ちにする。setter は独立に呼べるので、これを写す側で
        // 保証しないと「入力 8 MiB・展開 16 MiB」のような設定が作れてしまい、
        // 圧縮を経由するだけで入力上限を超える CFB を再パースさせられる。
        let max_decompressed = self.max_document_text_bytes.min(self.max_input_bytes);

        ParseLimits::DEFAULT
            .with_max_input_bytes(self.max_input_bytes)
            .with_max_decompressed_bytes(max_decompressed)
            .with_max_total_decompressed_bytes(max_decompressed)
            .with_max_decompression_ratio(self.max_expansion_ratio)
            .with_decompression_ratio_floor_bytes(EXPANSION_RATIO_FLOOR_BYTES)
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
            .with_max_input_bytes(456)
            .with_max_document_text_bytes(123);

        assert_eq!(limits.max_input_bytes(), 456);

        let parse_limits = limits.to_parse_limits();
        assert_eq!(parse_limits.max_input_bytes(), 456);
        assert_eq!(parse_limits.max_total_decompressed_bytes(), 123);
        assert!(
            parse_limits.check_input_size(457).is_err(),
            "入力上限を超えたら弾かれること"
        );
    }

    #[test]
    fn expansion_ratio_caps_compression_bombs() {
        // 展開後サイズだけでは「上限すれすれまで膨らむ小さな入力」を止められないため、
        // 比率でも切っていることを確かめる。比率下限（64 KiB）を超える大きさで試す。
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
    fn expansion_ratio_floor_is_tighter_than_upstream() {
        // 上流既定の下限は 1 MiB で、「1 MiB 未満なら圧縮率無制限」という穴になっていた。
        let parse_limits = JtdLimits::DEFAULT
            .with_max_expansion_ratio(2)
            .to_parse_limits();

        assert!(
            parse_limits.check_lh5_output_size(1, 512 * 1024).is_err(),
            "512 KiB への爆発は弾かれること（上流既定の 1 MiB 下限では素通りしていた）"
        );
        assert!(
            parse_limits.check_lh5_output_size(1, 32 * 1024).is_ok(),
            "下限以下の小さな出力は比率で弾かないこと"
        );
    }

    #[test]
    fn decompressed_limit_never_exceeds_input_limit() {
        // 展開上限が入力上限より大きいと、圧縮を経由するだけで入力上限を超える CFB を
        // 再パースさせられ、入力を締めた意味が消える。
        let limits = JtdLimits::DEFAULT;
        let parse_limits = limits.to_parse_limits();

        assert!(
            parse_limits.max_total_decompressed_bytes() <= limits.max_input_bytes(),
            "展開上限は入力上限を超えないこと"
        );
    }

    #[test]
    fn inconsistent_custom_limits_are_clamped() {
        // setter は独立に呼べるので、矛盾した組み合わせを作れてしまう。
        // 写す側で頭打ちにしていることを固定する。
        let limits = JtdLimits::DEFAULT
            .with_max_input_bytes(8 * MIB)
            .with_max_document_text_bytes(16 * MIB);

        let parse_limits = limits.to_parse_limits();

        assert_eq!(
            parse_limits.max_total_decompressed_bytes(),
            8 * MIB,
            "入力上限より大きい展開上限は入力上限へ切り詰められること"
        );
    }

    #[test]
    fn default_impl_matches_default_const() {
        assert_eq!(JtdLimits::default(), JtdLimits::DEFAULT);
    }
}
