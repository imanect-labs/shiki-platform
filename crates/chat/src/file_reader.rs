//! ファイルの種別（拡張子）から、検索用の索引を通さずに中身を読むツールを引く。
//!
//! 会話の添付は履歴に名前と node_id しか載らない。常時ロードの `doc_read` は索引から本文を
//! 組み立てるため、索引の無いファイル（大きな CSV・取り込み中・本文を取り出せない形式）では
//! 「読めない」で止まる。CSV・Office・ノート・スライドには原本を直接読むツールがあるが、
//! どれも遅延ロードなので、名前を挙げて案内しないとモデルが辿り着けない。

use agent_core::ToolName;

/// 種別ごとの読み取りツールと、対象のファイルを渡す引数名。知らない種別は `None`
/// （`doc_read` / `doc_search` で読む）。
pub(crate) fn direct_reader(name: &str) -> Option<(ToolName, &'static str)> {
    let (_, ext) = name.rsplit_once('.')?;
    match ext.to_ascii_lowercase().as_str() {
        "csv" => Some((ToolName::CsvQuery, "node_id")),
        "xlsx" | "ods" | "docx" | "odt" => Some((ToolName::OfficeRead, "file_id")),
        "md" => Some((ToolName::DocumentRead, "node_id")),
        "slide" => Some((ToolName::SlideRead, "node_id")),
        _ => None,
    }
}

/// モデル向けの案内（例「csv.query に node_id: … を渡して直接読める」）。
pub(crate) fn direct_read_hint(name: &str, id: &str) -> Option<String> {
    direct_reader(name)
        .map(|(tool, param)| format!("{} に {param}: {id} を渡して直接読める", tool.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 拡張子から読み取りツールを引く() {
        assert_eq!(
            direct_reader("customers-100000.CSV"),
            Some((ToolName::CsvQuery, "node_id"))
        );
        assert_eq!(
            direct_reader("見積.xlsx"),
            Some((ToolName::OfficeRead, "file_id"))
        );
        assert_eq!(
            direct_reader("議事録.md"),
            Some((ToolName::DocumentRead, "node_id"))
        );
        assert_eq!(
            direct_reader("提案.slide"),
            Some((ToolName::SlideRead, "node_id"))
        );
        // 索引で読む種別・拡張子なしは案内しない。
        assert_eq!(direct_reader("報告.pdf"), None);
        assert_eq!(direct_reader("README"), None);
    }

    #[test]
    fn 案内は引数名と_id_を含む() {
        assert_eq!(
            direct_read_hint("a.csv", "n1").as_deref(),
            Some("csv.query に node_id: n1 を渡して直接読める")
        );
        assert_eq!(direct_read_hint("a.pdf", "n1"), None);
    }
}
