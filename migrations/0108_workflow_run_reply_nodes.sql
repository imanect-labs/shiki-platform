-- 起動した本人の画面に返す結果（workflow.reply・GAP-09・D-28）。
--
-- 結果の正本は reply の step の出力（step_execution.output）で、別の場所へ写さない（実行履歴の保持期間に
-- 従う）。run には reply ノードの id（実行順＝トポロジカル順）だけを持たせ、読取口は ir_snapshot（≤1MB）を
-- 開かずに reply の step だけを引く。定数の既定値なので、既存の行は書き直されない。
alter table workflow_run add column reply_nodes text[] not null default '{}';
