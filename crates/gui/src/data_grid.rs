//! 画面（Markdown）の ```data_grid ブロック — 保存ビューをデータ束縛で描く表（Task 15.7）。
//!
//! 書き方（ミニアプリの画面）:
//!
//! ````markdown
//! ```data_grid
//! 承認待ちの経費
//! ```
//! ````
//!
//! 中身の 1 行目が、ミニアプリが束ねた保存ビューの別名（`MiniAppBody.data_views[].alias`）。
//! 画面は表を描くために `action_id = "data_grid:{別名}"` と params（ページ位置・件数だけ）を送り、
//! サーバーは**保存済みの版の画面とピン**から束縛（[`ActionBinding::DataView`]）を作り直して
//! 閲覧者本人の権限で実行する（クライアントから束縛・条件は送れない）。
//!
//! 読むのはトップレベルの閉じたフェンスだけ（Yui の ```ui と同じ CommonMark の規則）。引用・
//! 箇条書きの中のブロック、他の言語のコードブロックの中は読まない。
//!
//! [`ActionBinding::DataView`]: crate::ActionBinding::DataView

/// `data_grid` の action id の接頭辞（Yui のボタンの id `{16 桁の識別子}:{a1}` とは衝突しない）。
pub const DATA_GRID_ACTION_PREFIX: &str = "data_grid:";

/// 画面の中の ```data_grid ブロック 1 つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataGridBlock {
    /// 参照する保存ビューの別名（中身の 1 行目・前後の空白は除く）。空なら `None`。
    pub alias: Option<String>,
}

/// 表を描く操作の id（`data_grid:{別名}`）。
pub fn data_grid_action_id(alias: &str) -> String {
    format!("{DATA_GRID_ACTION_PREFIX}{alias}")
}

/// action id が表の描画なら別名を返す。
pub fn data_grid_alias(action_id: &str) -> Option<&str> {
    action_id
        .strip_prefix(DATA_GRID_ACTION_PREFIX)
        .filter(|a| !a.is_empty())
}

/// フェンスの開き（行頭の空白 0〜3・``` か ~~~ を 3 文字以上）を読む。
fn open_fence(line: &str) -> Option<(char, usize, &str)> {
    let t = line.trim_start_matches(' ');
    if line.len() - t.len() > 3 {
        return None;
    }
    let ch = t.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let n = t.chars().take_while(|c| *c == ch).count();
    if n < 3 {
        return None;
    }
    let info = &t[n * ch.len_utf8()..];
    // バッククォートのフェンスの情報文字列にはバッククォートを含められない（CommonMark）。
    if ch == '`' && info.contains('`') {
        return None;
    }
    Some((ch, n, info))
}

fn is_close(line: &str, ch: char, len: usize) -> bool {
    let t = line.trim_start_matches(' ');
    if line.len() - t.len() > 3 {
        return false;
    }
    let n = t.chars().take_while(|c| *c == ch).count();
    n >= len && t[n * ch.len_utf8()..].trim().is_empty()
}

fn is_data_grid_info(info: &str) -> bool {
    let lang: String = info
        .trim()
        .chars()
        .take_while(|c| !c.is_whitespace() && *c != '{')
        .collect::<String>()
        .to_lowercase();
    lang == "data_grid" || lang == "data-grid"
}

/// 画面から、閉じたトップレベルの ```data_grid ブロックを取り出す（出現順）。
pub fn data_grid_blocks(markdown: &str) -> Vec<DataGridBlock> {
    let lines: Vec<&str> = markdown
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let Some((ch, len, info)) = open_fence(lines[i]) else {
            i += 1;
            continue;
        };
        let mut j = i + 1;
        while j < lines.len() && !is_close(lines[j], ch, len) {
            j += 1;
        }
        let closed = j < lines.len();
        if closed && is_data_grid_info(info) {
            let alias = lines[i + 1..j]
                .iter()
                .map(|l| l.trim())
                .find(|l| !l.is_empty())
                .map(str::to_string);
            out.push(DataGridBlock { alias });
        }
        i = if closed { j + 1 } else { j };
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_top_level_closed_blocks_only() {
        let md = "# 経費\n\n```data_grid\n承認待ち\n```\n\n````md\n```data_grid\n中のもの\n```\n````\n\n> ```data_grid\n> 引用\n> ```\n\n~~~data-grid\n  全件  \n~~~\n\n```data_grid\n閉じていない\n";
        let blocks = data_grid_blocks(md);
        assert_eq!(
            blocks,
            vec![
                DataGridBlock {
                    alias: Some("承認待ち".into())
                },
                DataGridBlock {
                    alias: Some("全件".into())
                },
            ]
        );
    }

    #[test]
    fn empty_block_has_no_alias_and_other_langs_are_ignored() {
        assert_eq!(
            data_grid_blocks("```data_grid\n\n```"),
            vec![DataGridBlock { alias: None }]
        );
        assert!(data_grid_blocks("```ui\napp x\n```\n```json\n{}\n```").is_empty());
    }

    #[test]
    fn action_id_roundtrip() {
        let id = data_grid_action_id("承認待ち");
        assert_eq!(id, "data_grid:承認待ち");
        assert_eq!(data_grid_alias(&id), Some("承認待ち"));
        assert_eq!(data_grid_alias("data_grid:"), None);
        // Yui のボタンの id（16 桁の識別子:a1）は表の描画ではない。
        assert_eq!(data_grid_alias("0123456789abcdef:a1"), None);
    }
}
