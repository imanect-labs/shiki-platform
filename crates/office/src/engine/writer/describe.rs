//! Word ツールの説明文と入力スキーマ（#725）。操作の語彙は [`super::ops::DocOp`] が正。

use crate::engine::op_schema::{ops_items_schema, OpFields};

/// office.word_edit の説明。
pub const WORD_EDIT_DESCRIPTION: &str = "Word 文書（.docx / .odt）を、本文から見出し・表・箇条書き・グラフ・書式・スタイル・ページ設定・\
ヘッダー/フッター・目次まで編集する。ops は上から順に適用され、全部成功するか全部取り消される（途中で失敗しても文書は\
変わらない）。人が開いていれば画面へ即時反映され、人は 1 回の「元に戻す」で取り消せる。結果に最新のブロック一覧が返る\
（ブロック ID は編集で変わるので、以降は返ってきた ID を使う）。同じ ops の中では、差し込みの後も ID はそのまま使え、\
replace で置き換えたブロックの ID は置き換え後の中身を指す（after はその後ろ・表の操作はその表）。delete で消した ID は使えない。\n\
対象: office.read が返すブロック ID（\"b12-3fa9\"）か範囲（\"b12-3fa9:b15-77c1\"）、文書全体は \"all\"\
（作り直すときは replace {target:\"all\", html}）。本文は HTML で書く:\n\
見出し <h1>〜<h3>・段落 <p>・箇条書き <ul>/<ol><li>（入れ子可）・表 <table>（<th>・colspan・rowspan・<col width>）・\
<b> <i> <u> <s> <sup> <sub> <a href>・<br>・<img src=\"data:image/png;base64,...\">。CSS（style 属性や <style> のクラス・子孫セレクタ）は\
color / background-color / font-size / font-family / font-weight / text-align / text-indent / margin / line-height / border（表のセルのみ） / width / \
vertical-align / page-break-before が効く。段落スタイルは data-style=\"Heading 2\" のように office.read の一覧にある名前で付ける\
（文書の体裁がスタイルで揃うので、見出しや本文は直接書式よりスタイルを優先する）。data-style の無い <p> は標準の本文スタイル \
Standard になる（本文の書体・大きさ・行間は set_style で Standard を変える）。style に書いた文字の大きさや色はその段落だけの直接書式になり、\
後から set_style でスタイルを変えても上書きされない。表の幅・列幅（<col width>・width）は mm / pt / % / 数値（px）で書ける。段落の style の margin-top / margin-bottom は段落の前後の間隔、\
page-break-after: avoid は「次の段落と同じページに置く」（見出しを表や本文から離さない）になる。\
表と表の間には空の段落が 1 つ要る（隣り合う表は Word では 1 つにつながるため、エンジンが残す）。\
箇条書きは <ul>/<ol> の入れ子で書く（字下げは Word の既定に揃う・office.read も <ul>/<ol> で返す）。空行は <p></p>（段落の間隔は margin の方がよい）。外部の画像 URL は使えない。\n\
ops:\n\
- insert {after|before:ID か at:\"start\"|\"end\", html}\n\
- replace {target:ID または範囲, html}（範囲の各ブロックを消して、そこへ html を入れる。1 段落を data-style の無い \
1 段落で置き換えると元の段落スタイルを引き継ぐ。文言だけ直すなら replace_text の方が書式を保てる）\n\
- delete {target}\n\
- replace_text {find, replace, within?:範囲, all?, match_case?, whole_word?}（書式を保って文字列だけ置き換える。複数あるのに all が\
無ければ誤りになる）\n\
- format_text {find, within?, all?, font:{name?, size?, bold?, italic?, underline?, strike?, color?, highlight?}}\n\
- format_paragraph {target, include_tables?, clear_formatting?（先に直接書式を消してスタイルどおりにする）, style?, align?:left|center|right|justify, indent_left_mm?, first_line_indent_mm?, \
space_before_pt?, space_after_pt?, line_spacing?:倍率 または line_height_pt?:固定値, keep_with_next?, page_break_before?, font?}\n\
- set_style {name, family?:paragraph|character, based_on?, 段落の書式（format_paragraph と同じ項目）}（例 見出しの色を揃える・\
本文の書体を変える。無ければ作る）\n\
- page_setup {paper?:A4|A3|B5|B4|A5|letter, orientation?:portrait|landscape, margins_mm?:{top,right,bottom,left}}\n\
- header {content:{text, align?, size?} または null} / footer {同じ}（text に &[Page] &[Pages] &[Date] &[Title] &[File] を差し込める）\n\
- insert_toc {after|before|at?（既定は最初の見出しの前＝表紙の後ろ）, title?, levels?}（見出しから目次を作る） / update_toc {}（見出しを変えた後に目次を更新）\n\
- 表（table は表のブロック ID。同じ ops の前の操作で入れた表は \"last\"・セルは Excel と同じ A1 表記・行は 1 始まり）: insert_rows {table, after_row（0 で先頭）, count?, values?:[[...]]}\
（新しい行は隣の行の書式を写す） / delete_rows {table, rows:\"3\" または \"3:5\"} / set_cells {table, cell:\"B2\", values:[[...]]}\
（書式は保って文字だけ書く・null はそのセルを変えない） / format_cells {table, range:\"A1:D1\", fill?（null で塗りなし）, font?, align?, \
valign?:top|center|bottom, border?:{all|outline|inner|top|bottom|left|right|inside_horizontal|inside_vertical: \"thin\"|\"medium\"|\"thick\"|\
\"double\"|\"dashed\"|\"dotted\"|\"none\" または {style, color}}} / table_props {table, width_mm?, columns_mm?:[...], align?:left|center|right, \
repeat_header?:行数（ページをまたぐとき見出し行を繰り返す）, allow_row_split?, keep_with_next?} / style_table {table, theme?:blue|navy|green|gray|orange, \
header_rows?（既定 1）, total_row?:true または total_rows?:行数（小計・消費税・合計など末尾の集計行）, banded?（既定 true）, first_column?（1 列目を項目名として太字・薄い塗り。header_rows:0 と組み合わせると「項目｜内容」の表になる）}（見出しは濃い色に白の太字、縞、合計行は太字と上に二重線、全体に細い罫線、\
数値は右揃え）。既存の表は replace で作り直さずにこれらで直す（表の見た目が保たれる）。新しく作った表もまず style_table で整える\n\
- insert_chart {after|before|at?（既定は末尾）, type:column|bar|line|area|pie|doughnut, categories:[\"項目\", ...], \
series:[{name, values:[数値 または null（欠け）]}]（values は categories と同じ数）, title?, width_cm?（既定 15）, height_cm?（既定 9）, \
legend?, legend_position?:bottom|right|top|left|none, colors?:[\"#RRGGBB\"], data_labels?, y_number_format?, stacked?, percent?, x_title?, y_title?}\
（画像ではなく Word のグラフとして入り、値は文書の中に持つ。グラフは画像より insert_chart で入れる。bar は横棒・column は縦棒。\
円・ドーナツは系列 1 つ。office.read では［グラフ: 題］と見える）\n\
- page_break {before:ID} / title_page {enabled}（表紙: 1 ページ目にヘッダー・フッターを出さない）\n\
- track_changes {enabled}（有効にした後の insert / replace / delete / replace_text / format_text は変更履歴として残り、office.read の \
HTML で <ins> <del> として見える。replace_text で直す場合は find を変える最小の文字列にし within でブロックを絞ると差分が小さい） / \
accept_all_changes {} / reject_all_changes {}（未承認の変更をすべて確定・却下）\n\
作法: 見出しは h1〜h3（data-style で文書の見出しスタイルに合わせる）、本文は p、強調は必要な所だけ。表は見出し行を <th> にし、\
罫線と見出しの塗りを付ける。長い文書は節ごとに insert を分けず 1 回の ops にまとめてよい。";

/// office.word_edit の操作と、それぞれが受け付ける項目（`op` 以外・ops.rs の [`DocOp`] と一致させる）。
///
/// [`DocOp`]: super::ops::DocOp
pub const DOC_OP_FIELDS: OpFields = &[
    ("insert", &["after", "before", "at", "html"]),
    ("replace", &["target", "html"]),
    ("delete", &["target"]),
    (
        "replace_text",
        &[
            "find",
            "replace",
            "within",
            "all",
            "match_case",
            "whole_word",
        ],
    ),
    (
        "format_text",
        &["find", "within", "all", "match_case", "font"],
    ),
    // format_paragraph / set_style は段落書式（ParaSpec）を flatten で取り込む。
    (
        "format_paragraph",
        &[
            "target",
            "include_tables",
            "clear_formatting",
            "style",
            "align",
            "indent_left_mm",
            "first_line_indent_mm",
            "space_before_pt",
            "space_after_pt",
            "line_spacing",
            "line_height_pt",
            "keep_with_next",
            "page_break_before",
            "font",
        ],
    ),
    (
        "set_style",
        &[
            "name",
            "family",
            "based_on",
            "style",
            "align",
            "indent_left_mm",
            "first_line_indent_mm",
            "space_before_pt",
            "space_after_pt",
            "line_spacing",
            "line_height_pt",
            "keep_with_next",
            "page_break_before",
            "font",
        ],
    ),
    ("page_setup", &["paper", "orientation", "margins_mm"]),
    ("header", &["content"]),
    ("footer", &["content"]),
    ("insert_toc", &["after", "before", "at", "title", "levels"]),
    ("update_toc", &[]),
    ("page_break", &["before"]),
    ("track_changes", &["enabled"]),
    ("insert_rows", &["table", "after_row", "count", "values"]),
    ("delete_rows", &["table", "rows"]),
    ("set_cells", &["table", "cell", "values"]),
    (
        "format_cells",
        &[
            "table", "range", "fill", "font", "align", "valign", "border",
        ],
    ),
    (
        "table_props",
        &[
            "table",
            "width_mm",
            "columns_mm",
            "align",
            "repeat_header",
            "allow_row_split",
            "keep_with_next",
        ],
    ),
    (
        "style_table",
        &[
            "table",
            "theme",
            "header_rows",
            "total_row",
            "total_rows",
            "banded",
            "first_column",
        ],
    ),
    ("title_page", &["enabled"]),
    (
        "insert_chart",
        &[
            "after",
            "before",
            "at",
            "type",
            "categories",
            "series",
            "title",
            "width_cm",
            "height_cm",
            "legend",
            "legend_position",
            "colors",
            "data_labels",
            "y_number_format",
            "stacked",
            "percent",
            "x_title",
            "y_title",
        ],
    ),
    ("accept_all_changes", &[]),
    ("reject_all_changes", &[]),
];

/// office.word_edit の入力スキーマ。各操作の項目の形は説明文に書き、スキーマには項目名だけを
/// 型なしで並べる（理由は [`crate::engine::op_schema`]）。
pub fn word_edit_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "node_id": { "type": "string", "format": "uuid", "description": "対象ファイルのノード ID" },
            "summary": { "type": "string", "description": "この変更の要約（人の「元に戻す」の名前と承認画面に出る）" },
            "ops": {
                "type": "array",
                "minItems": 1,
                "description": "操作の並び（上から順に・全部成功か全部取り消し）。各操作の項目の形は説明文を参照",
                "items": ops_items_schema(DOC_OP_FIELDS, &[])
            }
        },
        "required": ["node_id", "summary", "ops"]
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::op_schema::probe;
    use crate::engine::writer::ops::DocOp;

    #[test]
    fn field_table_matches_the_doc_op_enum() {
        let table: Vec<&str> = DOC_OP_FIELDS.iter().map(|(op, _)| *op).collect();
        assert_eq!(
            probe::variants::<DocOp>(),
            table,
            "操作名が ops.rs と食い違う"
        );
        for (op, fields) in DOC_OP_FIELDS {
            match probe::fields::<DocOp>(op) {
                Some(actual) => assert_eq!(&actual, fields, "{op} の項目が ops.rs と食い違う"),
                // flatten を含む操作は列挙できないので、表の項目がどれも受け付けられることを確かめる。
                None => {
                    for f in *fields {
                        assert!(probe::accepts::<DocOp>(op, f), "{op} は {f} を受け付けない");
                    }
                }
            }
        }
    }

    #[test]
    fn schema_declares_every_field_without_a_type() {
        let s = word_edit_schema();
        let props = &s["properties"]["ops"]["items"]["properties"];
        assert_eq!(
            props["op"]["enum"].as_array().unwrap().len(),
            DOC_OP_FIELDS.len()
        );
        for f in ["html", "target", "series", "categories", "line_spacing"] {
            assert!(props.get(f).is_some(), "{f} がスキーマに無い");
            assert!(props[f].get("type").is_none(), "{f} は型なしにする");
        }
    }
}
