//! Excel ツールの説明文と入力スキーマ（#725）。
//!
//! 単一定義: チャットのツール（office.read / office.excel_edit）と評価台が同じものを使う。
//! 操作の語彙は [`super::ops::SheetOp`] が正で、ここの列挙とずれないことをテストで確かめる。

/// office.read の説明。
pub const READ_DESCRIPTION: &str =
"Excel ブック（.xlsx / .ods）を読む。人が開いて編集中なら未保存の内容も含めて読める。\
         range を省くと、ブックの概要（シート一覧・使用範囲・グラフ・名前）と作業中シートの左上を返す。\
         range（例 \"売上!A1:H40\"・\"A:F\"）を指定すると、その範囲の値（画面の表示どおり）・数式（Excel の書き方）・\
         書式（文書の既定と違うものだけ）・結合・列幅を返す。details=true で条件付き書式・グラフ・印刷設定・\
         ウィンドウ枠の固定も返す。編集（office.excel_edit）の前に、表の位置・見出し・書式を必ずこれで確かめる。";

/// office.excel_edit の説明（操作の語彙は単一定義の `SheetOp` と対で保つ・テストで照合）。
pub const EXCEL_EDIT_DESCRIPTION: &str = "Excel ブック（.xlsx / .ods）を、値・数式から書式・罫線・結合・列幅・条件付き書式・入力規則・\
グラフ・印刷設定・シート構成まで編集する。ops は上から順に適用され、全部成功するか全部取り消される\
（途中で失敗しても文書は変わらない）。人が開いていれば画面へ即時反映され、人は 1 回の「元に戻す」で取り消せる。\
結果には書いた範囲の現在の値（数式の計算結果）と、エラー・列幅不足の指摘が返る。必ず確かめ、必要なら直す。\n\
書き方: 範囲は Excel の A1 表記（\"B2:D10\"・\"'集計 2'!A1\"・列 \"B:D\"・行 \"3:5\"）。シート名を省くと sheet（無ければ作業中の\
シート）。色は \"#RRGGBB\"。**指定しない項目は変えない**。数式は Excel の書き方（区切りは ,）。\n\
ops:\n\
- set_values {range, values:[[...]]} 値と数式。range は左上セルだけでよい。数値は数値のまま、数式は \"=SUM(B2:B9)\"、\
日付は {\"date\":\"2026-04-01\"}、= で始まる文字列は {\"text\":\"=…\"}、null はそのセルを変えない\n\
- fill_formula {range, formula} 左上セル用の数式を範囲全体へ相対参照で展開（例 range \"E2:E20\", formula \"=B2*C2\"）\n\
- format {range（配列可）, number_format?, font?{name,size,bold,italic,underline,strike,color}, fill?（null で塗りなし）, \
align?{horizontal:left|center|right|justify, vertical:top|center|bottom, wrap, shrink, indent, rotate}, \
border?{all|outline|inner|top|bottom|left|right|inside_horizontal|inside_vertical: \"hair\"|\"thin\"|\"medium\"|\"thick\"|\"double\"|\
\"dashed\"|\"dotted\"|\"none\" または {style, color}}}。表示形式の例: \"#,##0\" \"#,##0.00\" \"0.0%\" \"yyyy/mm/dd\" \"¥#,##0\" \
\"#,##0;[Red]-#,##0\" \"[$-411]ggge年m月d日\"\n\
- merge {range, center?} / unmerge {range}\n\
- column_width {columns:\"A:C\", width:文字数 または \"auto\"} / row_height {rows:\"1:2\", height:pt または \"auto\"}\n\
- insert_rows {before:行番号, count} / delete_rows {rows} / insert_columns {before:\"C\", count} / delete_columns {columns} / \
hide_rows・show_rows {rows} / hide_columns・show_columns {columns}\n\
- clear {range, what:contents|formats|all}\n\
- sort {range, by:[{column:\"E\", order:asc|desc}], has_header?}\n\
- autofilter {range} / {sheet, enabled:false}\n\
- freeze {cell:\"B2\"}（そのセルの左と上を固定。null で解除）\n\
- add_sheet {name, position?} / rename_sheet {sheet, name} / delete_sheet {sheet} / move_sheet {sheet, position} / \
sheet_props {sheet, tab_color?, visible?}\n\
- conditional_format {range, when:{operator:\"<\"|\"<=\"|\">\"|\">=\"|\"=\"|\"<>\"|\"between\"|\"not_between\", value, value2?} \
または {formula:\"=$E2>1000\"（範囲の左上基準）}, style:{font_color?, fill?, bold?}} / clear_conditional_formats {range}\n\
- data_validation {range, rule, input_message?, error_message?, allow_blank?}。rule は {type:\"list\", items:[...]} / \
{type:\"list\", source:\"マスタ!A2:A20\"} / {type:\"whole_number\"|\"decimal\"|\"text_length\", operator, value, value2?} / \
{type:\"date\", operator, value:\"2026-01-01\"} / {type:\"custom\", formula}\n\
- define_name {name, range}（range:null で削除）\n\
- add_chart {name, type:column|bar|line|area|pie|doughnut|scatter, data:\"A1:D13\"（先頭行=系列名・先頭列=項目）, anchor:\"G2\", \
width_cm?, height_cm?, title?, legend?, series?:columns|rows, stacked?, percent?, x_title?, y_title?} / delete_chart {name}\n\
- page_setup {orientation?:portrait|landscape, paper?:A4|A3|B5|B4|A5|letter, fit_to?{width,height}（0=その方向は制限なし）, scale?, \
margins_cm?{top,right,bottom,left}, center_horizontally?, print_area?（null で解除）, repeat_rows?:\"1:2\", gridlines?}\n\
作法: 見出し行は太字・塗り・中央揃え、数値は表示形式で桁区切り（文字列にしない）、合計行は上に罫線、列幅は内容に合わせる。\
集計は値を暗算して書かず数式にする。表を作ったら読みやすく整えてから終える。";

/// office.read の入力スキーマ。
pub fn read_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "node_id": { "type": "string", "format": "uuid", "description": "対象ファイルのノード ID" },
            "range": { "type": "string", "description": "読む範囲（A1 表記・シート名つき可）。省略で概要" },
            "sheet": { "type": "string", "description": "range にシート名が無いときのシート" },
            "details": { "type": "boolean", "description": "条件付き書式・グラフ・印刷設定・固定も読む" },
            "max_cells": { "type": "integer", "description": "返すセル数の上限（既定 1500・最大 5000）" }
        },
        "required": ["node_id"]
    })
}

/// office.excel_edit の入力スキーマ（op の中身は説明文に書き、スキーマは op 名の列挙だけにする:
/// union 型の詳細スキーマは一部のプロバイダ（DeepSeek 等）の function 検証が拒否するため）。
pub fn excel_edit_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "node_id": { "type": "string", "format": "uuid", "description": "対象ファイルのノード ID" },
            "summary": { "type": "string", "description": "この変更の要約（人の「元に戻す」の名前と承認画面に出る・例「売上表に合計行と書式を追加」）" },
            "sheet": { "type": "string", "description": "範囲にシート名が無いときのシート（省略で作業中のシート）" },
            "ops": {
                "type": "array",
                "minItems": 1,
                "description": "操作の並び（上から順に・全部成功か全部取り消し）。各操作の項目は説明文を参照",
                "items": {
                    "type": "object",
                    "properties": {
                        "op": { "type": "string", "enum": [
                            "set_values", "fill_formula", "format", "merge", "unmerge", "column_width", "row_height",
                            "insert_rows", "delete_rows", "insert_columns", "delete_columns", "hide_rows", "show_rows",
                            "hide_columns", "show_columns", "clear", "sort", "autofilter", "freeze", "add_sheet",
                            "rename_sheet", "delete_sheet", "move_sheet", "sheet_props", "conditional_format",
                            "clear_conditional_formats", "data_validation", "define_name", "add_chart", "delete_chart",
                            "page_setup"
                        ] }
                    },
                    "required": ["op"]
                }
            }
        },
        "required": ["node_id", "summary", "ops"]
    })
}
