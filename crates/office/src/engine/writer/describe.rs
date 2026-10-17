//! Word ツールの説明文と入力スキーマ（#725）。操作の語彙は [`super::ops::DocOp`] が正。

/// office.word_edit の説明。
pub const WORD_EDIT_DESCRIPTION: &str = "Word 文書（.docx / .odt）を、本文から見出し・表・箇条書き・書式・スタイル・ページ設定・\
ヘッダー/フッター・目次まで編集する。ops は上から順に適用され、全部成功するか全部取り消される（途中で失敗しても文書は\
変わらない）。人が開いていれば画面へ即時反映され、人は 1 回の「元に戻す」で取り消せる。結果に最新のブロック一覧が返る\
（ブロック ID は編集で変わるので、以降は返ってきた ID を使う）。同じ ops の中では、replace / delete で消したブロック以外の \
ID は差し込みの後もそのまま使える。\n\
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
- insert_toc {after|before|at?（既定は先頭）, title?, levels?}（見出しから目次を作る） / update_toc {}（見出しを変えた後に目次を更新）\n\
- 表（table は表のブロック ID・セルは Excel と同じ A1 表記・行は 1 始まり）: insert_rows {table, after_row（0 で先頭）, count?, values?:[[...]]}\
（新しい行は隣の行の書式を写す） / delete_rows {table, rows:\"3\" または \"3:5\"} / set_cells {table, cell:\"B2\", values:[[...]]}\
（書式は保って文字だけ書く・null はそのセルを変えない） / format_cells {table, range:\"A1:D1\", fill?（null で塗りなし）, font?, align?, \
valign?:top|center|bottom, border?:{all|outline|inner|top|bottom|left|right|inside_horizontal|inside_vertical: \"thin\"|\"medium\"|\"thick\"|\
\"double\"|\"dashed\"|\"dotted\"|\"none\" または {style, color}}} / table_props {table, width_mm?, columns_mm?:[...], align?:left|center|right, \
repeat_header?:行数（ページをまたぐとき見出し行を繰り返す）, allow_row_split?, keep_with_next?} / style_table {table, theme?:blue|navy|green|gray|orange, \
header_rows?（既定 1）, total_row?:true または total_rows?:行数（小計・消費税・合計など末尾の集計行）, banded?（既定 true）, first_column?（1 列目を項目名として太字・薄い塗り。header_rows:0 と組み合わせると「項目｜内容」の表になる）}（見出しは濃い色に白の太字、縞、合計行は太字と上に二重線、全体に細い罫線、\
数値は右揃え）。既存の表は replace で作り直さずにこれらで直す（表の見た目が保たれる）。新しく作った表もまず style_table で整える\n\
- page_break {before:ID} / title_page {enabled}（表紙: 1 ページ目にヘッダー・フッターを出さない）\n\
- track_changes {enabled}（有効にした後の insert / replace / delete / replace_text / format_text は変更履歴として残り、office.read の \
HTML で <ins> <del> として見える。replace_text で直す場合は find を変える最小の文字列にし within でブロックを絞ると差分が小さい） / \
accept_all_changes {} / reject_all_changes {}（未承認の変更をすべて確定・却下）\n\
作法: 見出しは h1〜h3（data-style で文書の見出しスタイルに合わせる）、本文は p、強調は必要な所だけ。表は見出し行を <th> にし、\
罫線と見出しの塗りを付ける。長い文書は節ごとに insert を分けず 1 回の ops にまとめてよい。";

/// office.word_edit の入力スキーマ（op の中身は説明文に書き、スキーマは op 名の列挙だけにする）。
pub fn word_edit_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "node_id": { "type": "string", "format": "uuid", "description": "対象ファイルのノード ID" },
            "summary": { "type": "string", "description": "この変更の要約（人の「元に戻す」の名前と承認画面に出る）" },
            "ops": {
                "type": "array",
                "minItems": 1,
                "description": "操作の並び（上から順に・全部成功か全部取り消し）。各操作の項目は説明文を参照",
                "items": {
                    "type": "object",
                    "properties": {
                        "op": { "type": "string", "enum": [
                            "insert", "replace", "delete", "replace_text", "format_text", "format_paragraph", "set_style",
                            "page_setup", "header", "footer", "insert_toc", "update_toc", "page_break", "title_page", "insert_rows", "delete_rows", "set_cells",
                            "format_cells", "table_props", "style_table", "track_changes",
                            "accept_all_changes", "reject_all_changes"
                        ] }
                    },
                    "required": ["op"]
                }
            }
        },
        "required": ["node_id", "summary", "ops"]
    })
}
