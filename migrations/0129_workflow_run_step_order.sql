-- 実行の詳細で、同じ時刻に書かれた step を実行の順に並べるためのノードの順（BUG-117）。
--
-- 実行の詳細は step を状態の変わった時刻（updated_at）の順に並べる。1 つの前進の Tx（ブロックの完了と、
-- 通らなかった後続の skip）で書いた step は同じ時刻になるので、その中を線の前のブロックが先になる順
-- （本体のトポロジカル順・map の後ろに領域の中の順）で並べる。run にノードの id の順だけを持たせ、読取口は
-- ir_snapshot（≤1MB）を開かない（reply_nodes と同じ・0121）。定数の既定値なので、既存の行は書き直されない
-- （既存の run は今までどおり step_path の順で並ぶ）。
alter table workflow_run add column step_order text[] not null default '{}';
