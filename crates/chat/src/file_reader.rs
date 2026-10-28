//! ファイルの種別（拡張子）から、検索用の索引を通さずに中身を読むツールを引く。
//!
//! 会話の添付は履歴に名前と node_id しか載らない。常時ロードの `doc_read` は索引から本文を
//! 組み立てるため、索引の無いファイル（大きな CSV・取り込み中・本文を取り出せない形式）では
//! 「読めない」で止まる。CSV・Office・ノート・スライドには原本を直接読むツールがあるが、
//! どれも遅延ロードなので、名前を挙げて案内しないとモデルが辿り着けない。
//! 引数はどれも `node_id`（各ツールの入力スキーマ）。

use agent_core::ToolName;

/// 種別ごとの読み取りツール。知らない種別は `None`（`doc_read` / `doc_search` で読む）。
pub(crate) fn direct_reader(name: &str) -> Option<ToolName> {
    let (_, ext) = name.rsplit_once('.')?;
    match ext.to_ascii_lowercase().as_str() {
        "csv" => Some(ToolName::CsvQuery),
        "xlsx" | "ods" | "docx" | "odt" => Some(ToolName::OfficeRead),
        "md" => Some(ToolName::DocumentRead),
        "slide" => Some(ToolName::SlideRead),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 拡張子から読み取りツールを引く() {
        assert_eq!(
            direct_reader("customers-100000.CSV"),
            Some(ToolName::CsvQuery)
        );
        assert_eq!(direct_reader("見積.xlsx"), Some(ToolName::OfficeRead));
        assert_eq!(direct_reader("議事録.md"), Some(ToolName::DocumentRead));
        assert_eq!(direct_reader("提案.slide"), Some(ToolName::SlideRead));
        // 索引で読む種別・拡張子なしは案内しない。
        assert_eq!(direct_reader("報告.pdf"), None);
        assert_eq!(direct_reader("README"), None);
    }
}
