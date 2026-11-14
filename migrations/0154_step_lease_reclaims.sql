-- リースが切れて拾い直した回数を step に持たせる（BUG-215 の残り）。
--
-- リース失効の回収（sweeper）は、落ちたワーカーの実行を試す回数に数えないよう attempt を戻して ready にする
-- （engine.md §9.5）。そのため、ブロックの実行より外で決まって panic する step や、ワーカーが誤りを返して前進できない
-- step（IR を読めないなど）は、リースの期間ごとに上限なく拾い直されていた。
--
--   lease_reclaims = この step のリースが切れて拾い直した回数（sweeper が ready へ戻すたびに +1）。上限
--                    （`MAX_LEASE_RECLAIMS`）に達した step のリースがまた切れたら、拾い直さずに internal の失敗で
--                    確定し、run も失敗で終える。失敗したところから再開したときに 0 へ戻す。
--
-- 既定値の 0 で足すので、前からある step は 0 から数える（PostgreSQL 11 以降は既存の行を書き直さない）。
alter table step_execution
    add column lease_reclaims int not null default 0;
