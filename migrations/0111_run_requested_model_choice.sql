-- 発話で利用者が送ったモデル選択そのもの（入力欄の選択の端末間引き継ぎ・#1050）。
--
-- `model` / `effort` / `model_auto` は組織の設定で解決した後の値で、既定が特定のモデルの組織では
-- 「Auto」や「既定のまま思考の深さだけ変える」も `model = 既定, model_auto = false` になり、
-- 明示的に選んだモデルと区別できない。別の端末で会話を開いたときの入力欄の初期値は、解決前の
-- 選択（`{"model": <id|null>, "effort": <low|medium|high|null>}`）から作る。
-- NULL はこの列以前の run（解決後の値から推定する）。
alter table generation_run add column if not exists requested_choice jsonb;
