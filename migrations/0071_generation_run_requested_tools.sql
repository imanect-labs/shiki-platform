-- 発話ごとのツール指定（#734）。ユーザーが「この発話ではこのツールを使って」と選んだツール名を
-- run 行へスナップショットする（skill_pins / model と同じく、run 行が生成材料の単一ソース＝
-- claim が 1 行で完結する）。
--
-- 値はツール語彙（agent-core `ToolName`）の名前の JSON 配列（例: ["web_search", "csv.query"]）。
-- 検証（未知名・重複・件数上限）は API 層で済ませてから入れる。ワーカーは読み出し時にも語彙で
-- 照合し、後のリリースで消えた名前は黙って落とす（claim を失敗させない）。
--
-- これは**誘導**であり権限ではない: 提示ツール集合・承認ポリシ・認可は変えない（tool search の
-- 常時ロードと system の注記に使うだけ）。
--
-- 定数 default の列追加はメタデータだけで済む（既存行の書き換えは起きない）。
alter table generation_run
    add column requested_tools jsonb not null default '[]'::jsonb;
