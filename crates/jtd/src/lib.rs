//! 一太郎（JTD）ファイルの読み取り（トラックJTD）。
//!
//! 日本の官公庁・学会は現在も一太郎形式で様式を配布しており、これを shiki で扱えるようにする。
//! **Collabora / LibreOffice は JTD を読めない**（Linux 版に一太郎フィルタが無く、
//! `--convert-to` は CFB の生バイトをテキストとして吐くだけ）ため、既存の Office 経路は流用できず、
//! 変換を自前で持つ。最終的な出口は docx で、RAG・プレビュー・共有はそこから既存経路に乗る。
//!
//! # 層の分け方
//!
//! - CFB コンテナの読解と `DocumentText` のトークン化は所有フォーク
//!   [`rjtd_core`]（`vendor/openjtd`・Apache-2.0）に任せる。
//! - **表・罫線・ページ幾何の解読と OOXML への写像**が我々の担当分（後続タスク）。
//!   上流は本文テキストまでで、表構造とページ幾何は未解読と明示している。
//!
//! 上流の型は公開 API に出さない。将来 rjtd を別実装へ差し替えても、この crate の
//! 公開型が変わらないようにするため（CLAUDE.md「差し替えはトレイト裏で」）。
//!
//! # 入力は敵対的として扱う
//!
//! JTD はユーザーがアップロードした外部由来のバイナリで、しかも docx/pdf と違って
//! パースが **shiki-server のプロセス内**で走る。よって
//!
//! - 資源上限を [`JtdLimits`] で必ず掛ける（既定は上流より厳しい値）。
//! - 上流のパニックが API を巻き込まないよう境界で捕捉して [`JtdError::Malformed`] に落とす。
//! - 失敗理由の詳細は公開せず `tracing` に落とす（フォーマット解析のオラクルにしない）。
//!
//! ただし **`catch_unwind` は万能ではない**。捕まえられるのは unwind するパニックだけで、
//! **abort**（Rust の OOM とスタックオーバーフローは unwind しない）と**二次時間・無限ループ**は
//! 素通りする。実際、独立レビューで 3 種類とも踏み抜いた:
//!
//! - 1 KiB の CFB が DIFAT 走査を暴走させ、2 GiB の確保失敗で abort（`patches/0001`）。
//! - 約 1 MiB の CFB がディレクトリ走査の再帰でスタックオーバーフロー → abort（`patches/0002`）。
//! - 512 KiB の CFB が埋め込みテキストの重複排除を O(N²) で回した（`patches/0003`）。
//!
//! よって細工入力に対する要件は「エラーを返すこと」ではなく
//! **「有界な時間とメモリで返ること」**とし、`tests/adversarial_it.rs` で経過時間ごと固定する。

mod error;
mod limits;

use std::panic::{catch_unwind, AssertUnwindSafe};

use rjtd_core::container::Container;
use rjtd_core::document_text::{
    read_document_text_payload_with_limits, COMPRESSED_DOCUMENT_PATH, DOCUMENT_TEXT_PATH,
    EMBEDDED_DOCUMENT_TEXT_PATH,
};

pub use error::{JtdError, JtdLimitKind};
pub use limits::JtdLimits;

/// CFB（OLE 複合文書）のシグネチャ。一太郎 8〜13 系はこの容れ物を使う。
const CFB_MAGIC: &[u8; 8] = b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1";

/// 認識した JTD の系統。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JtdFormat {
    /// CFB ＋ `/DocumentText`。一太郎 8〜13 系の本流で、現行のサポート対象。
    DocumentText,
    /// CFB ＋ `/JSCompDocument`（LHA 圧縮）。`.jttc` 等。読めるが今回のスコープ外。
    CompressedDocument,
    /// `/DocumentText` を持たず、埋め込み断片からのみ本文が拾える変種。
    EmbeddedDocumentText,
}

impl JtdFormat {
    /// 監査ログ・メトリクス用の安定した識別子。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DocumentText => "document-text",
            Self::CompressedDocument => "compressed-document",
            Self::EmbeddedDocumentText => "embedded-document-text",
        }
    }
}

/// CFB 内のストリーム 1 本の素性（診断・解読作業用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JtdStream {
    path: String,
    size: u64,
}

impl JtdStream {
    /// ストリームのパス（`/DocumentText` など）。
    pub fn path(&self) -> &str {
        &self.path
    }

    /// バイト数。
    pub fn size(&self) -> u64 {
        self.size
    }
}

/// 読み取り済みの JTD ファイル。
///
/// この型が持つのは「本文まで到達できた」という事実であり、レイアウトはまだ持たない。
/// 表・罫線・ページ幾何の解読は後続タスクでこの上に載せる。
#[derive(Debug)]
pub struct JtdFile {
    format: JtdFormat,
    streams: Vec<JtdStream>,
    document_text: Vec<u8>,
    plain_text: String,
}

impl JtdFile {
    /// 既定の資源上限で読む。
    pub fn open(bytes: &[u8]) -> Result<Self, JtdError> {
        Self::open_with_limits(bytes, JtdLimits::DEFAULT)
    }

    /// 資源上限を明示して読む。
    ///
    /// 上限チェックは**バイト列を受け取った後**に走る。呼び出し側が既に確保したメモリは
    /// 取り戻せないので、アップロード経路側のサイズ上限と二重に掛けること。
    pub fn open_with_limits(bytes: &[u8], limits: JtdLimits) -> Result<Self, JtdError> {
        if bytes.len() > limits.max_input_bytes() {
            tracing::debug!(
                actual = bytes.len(),
                limit = limits.max_input_bytes(),
                "jtd: 入力が上限を超えています"
            );
            return Err(JtdError::TooLarge(JtdLimitKind::Input));
        }
        if !bytes.starts_with(CFB_MAGIC) {
            return Err(JtdError::NotJtd);
        }

        // 上流の `detect_format` は使わない。内部でもう一度 CFB を開いて `/DocumentText` を
        // 丸ごと読んでは捨てるだけで、ここで得るストリーム一覧から同じ判定ができる。
        //
        // それでも CFB は 2 回開く（ここと、下の本文読み）。上流に「解析済みの `Container` から
        // ストリームを読む」入口が無いためで、これを 1 回にするのは rjtd-core の API 追加＝
        // 上流へ出す変更になる。実物（60〜100 KB）では無視できるコストなので、
        // 中間モデルを起こす JTD.2 で本文読みを自前に置き換えるときに併せて畳む。
        let container =
            guard(|| Container::from_cfb_bytes(bytes))?.map_err(|error| map_upstream(&error))?;
        let streams: Vec<JtdStream> = container
            .entries()
            .iter()
            .map(|entry| JtdStream {
                path: entry.path().to_string(),
                size: entry.size(),
            })
            .collect();
        drop(container);

        let parse_limits = limits.to_parse_limits();
        let payload = guard(|| read_document_text_payload_with_limits(bytes, parse_limits))?
            .map_err(|error| map_upstream(&error))?;
        let format = classify(payload.source_name(), &streams);

        Ok(JtdFile {
            format,
            streams,
            document_text: payload.bytes().to_vec(),
            plain_text: payload.text().to_string(),
        })
    }

    /// 認識した系統。
    pub fn format(&self) -> JtdFormat {
        self.format
    }

    /// CFB 内のストリーム一覧（ストレージも含む）。
    pub fn streams(&self) -> &[JtdStream] {
        &self.streams
    }

    /// `/DocumentText` ストリームの生バイト列。
    ///
    /// 先頭 8 バイトは `SsmgV.01`、以降は UTF-16BE のテキストと、`0x001C` で開き `0x001F` で
    /// 閉じる制御レコードが交互に並ぶ。**単一のストリーム**であることが保証されるので、
    /// 後続タスクのレイアウト解読はここを入力にする。
    ///
    /// [`JtdFormat::EmbeddedDocumentText`] では `None`。その変種の本文は複数の断片を
    /// つないだもので、単一ストリームとして読むと断片ヘッダと境界を誤読する
    /// （[`Self::embedded_fragments`] で取る）。
    pub fn document_text_bytes(&self) -> Option<&[u8]> {
        match self.format {
            JtdFormat::DocumentText | JtdFormat::CompressedDocument => Some(&self.document_text),
            JtdFormat::EmbeddedDocumentText => None,
        }
    }

    /// 埋め込み断片をつないだバイト列（[`JtdFormat::EmbeddedDocumentText`] のときだけ `Some`）。
    ///
    /// **単一ストリームではない。** `SsmgV.01` で始まる断片が `0x0000` 区切りで並ぶ。
    /// レイアウト解読に使うなら、まず断片へ切り分けること。
    pub fn embedded_fragments(&self) -> Option<&[u8]> {
        match self.format {
            JtdFormat::EmbeddedDocumentText => Some(&self.document_text),
            JtdFormat::DocumentText | JtdFormat::CompressedDocument => None,
        }
    }

    /// 本文テキスト（読み順）。
    ///
    /// **表は構造を失って読み順のテキストになる。** セルの升目は復元されない。
    /// 表を表として扱えるようにするのは後続タスク。
    pub fn plain_text(&self) -> &str {
        &self.plain_text
    }
}

/// 本文がどこから来たかで系統を決める。本文の取得に成功した後にだけ呼ぶ。
///
/// **ストリームの有無だけで決めない。** 壊れた `/JSCompDocument` と有効な埋め込み断片が
/// 同居していると、上流は展開に失敗して埋め込み断片へフォールバックする。存在だけを見ると
/// `CompressedDocument` と名乗ってしまい、監査ラベルが実際に読んだ経路と食い違う。
/// 埋め込み経由かどうかは payload の出所が教えてくれるので、そちらを優先する。
///
/// 直接読みと圧縮経由はどちらも出所が `/DocumentText` になるため、この 2 つの区別だけは
/// ストリームの有無で行う。
fn classify(source_name: &str, streams: &[JtdStream]) -> JtdFormat {
    if source_name == EMBEDDED_DOCUMENT_TEXT_PATH {
        return JtdFormat::EmbeddedDocumentText;
    }

    let has = |path: &str| streams.iter().any(|stream| stream.path() == path);
    if has(DOCUMENT_TEXT_PATH) {
        JtdFormat::DocumentText
    } else if has(COMPRESSED_DOCUMENT_PATH) {
        JtdFormat::CompressedDocument
    } else {
        JtdFormat::EmbeddedDocumentText
    }
}

/// 上流呼び出しのパニックを境界で捕まえる。
///
/// `rjtd_core` は `unsafe_code = forbid` なので未定義動作は無いが、細工されたオフセットで
/// スライス範囲外パニックが出る可能性は残る。プロセス内パースなので、それを 1 リクエストの
/// エラーに閉じ込める。
fn guard<T>(f: impl FnOnce() -> T) -> Result<T, JtdError> {
    catch_unwind(AssertUnwindSafe(f)).map_err(|_| {
        tracing::warn!("jtd: 上流パーサがパニックしました（壊れた入力として扱います）");
        JtdError::Malformed
    })
}

/// 上流エラーを我々の語彙へ写しつつ、詳細を `tracing` に残す。
fn map_upstream(error: &rjtd_core::Error) -> JtdError {
    tracing::debug!(%error, "jtd: 上流パーサがエラーを返しました");
    JtdError::from_upstream(error)
}

#[cfg(test)]
mod tests;
