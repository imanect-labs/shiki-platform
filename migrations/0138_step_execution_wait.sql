-- 試す回数を使わずに待ってやり直している step の理由と、待ち始めた時刻（D-125・GAP-50）。
--
-- 「ファイルを読む」で PDF・Office の本文の取り込みが終わっていない（index_pending）とき、step を失敗にせず、
-- 試す回数を使わずに間隔を伸ばしながら読み直す。待ち始めてから上限（30 分）を過ぎたら index_pending で失敗にする。
--
-- - wait_code: いま待っている理由の code（待って読み直す ready の間だけ持つ・実行の詳細が「取り込みを待っています」と
--   言う材料）。ほかの理由のやり直しに移ったら NULL にする。
-- - wait_since: 待ち始めた時刻（上限の起点）。ほかの理由のやり直しを挟んでも残し、step が終わったとき・失敗した
--   ところから再開したときに NULL にする（再開は待ち直す）。
--
-- どちらも既定値の無い NULL の列なので、既存の行は書き直されない。
alter table step_execution
    add column wait_code text,
    add column wait_since timestamptz;
