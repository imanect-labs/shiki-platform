-- 回答が途中で止まった理由を、アシスタントのメッセージに残す（GAP-106）。
--
-- チャットの生成ワーカーは、これまで agent-core の停止理由（出力の上限での打ち切り・手数や費用の上限・
-- 失敗の繰り返しの検知）を捨てて run を done にしていたため、利用者は回答が途中で切れたことが分からなかった。
-- 画面は回答の下に注記を出す。読み直しても出るように、確定（本文の書き戻し）と同じトランザクションで書く。
--
--   stop_reason = 止まった理由（`chat::StopReason`・SSE の done イベントと同じ値）
--     truncated     … 1 回の応答の出力の上限で切れた
--     step_limit    … 手数の上限に達した
--     time_limit    … 時間の上限に達した
--     token_limit   … 扱うトークン量の上限に達した
--     cost_limit    … 費用の上限に達した
--     loop_detected … 同じ失敗の繰り返しを検知して止めた
--   自然に終わった回答・利用者が止めた回答・失敗した回答・利用者の発話は null。
--
-- 前からあるメッセージは埋めない（null のまま・今までどおり注記なしで表示する）。
alter table message add column stop_reason text;

-- 新しく書く行だけを確かめる（not valid。前からの行は全て null なので、走査で ALTER のロックを延ばさない）。
alter table message
    add constraint message_stop_reason_check
        check (stop_reason in (
            'truncated', 'step_limit', 'time_limit', 'token_limit', 'cost_limit', 'loop_detected'
        ))
        not valid;
