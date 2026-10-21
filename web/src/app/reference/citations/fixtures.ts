/// 引用表示ギャラリーの固定フィクスチャ（issue #505）。
///
/// 想定: 「育休の取得条件と申請期限を教えて」に doc_search を 2 回呼んで答えた。
/// 引用は到着順に 8 件（1 回目 4 件・2 回目 4 件）。本文はそのうち 1,2,5,6,7 を引用し、
/// 3,4,8 は検索したが使わなかった。番号は応答内の通し番号（サーバ側の採番修正後の姿）。
import type { Citation } from "@/lib/chat-api";
import type { NodeMeta } from "@/lib/node-name-cache";

const F_RULES = "f-rules";
const F_KNOWLEDGE = "f-knowledge";
const F_AGREEMENT = "f-agreement";

export const METAS: Record<string, NodeMeta> = {
  [F_RULES]: { name: "規程", parentId: null, updatedAt: null, contentType: null },
  [F_KNOWLEDGE]: { name: "ナレッジ", parentId: null, updatedAt: null, contentType: null },
  [F_AGREEMENT]: { name: "協定書", parentId: null, updatedAt: null, contentType: null },
  "d-rule": {
    name: "就業規則_2026改訂.docx",
    parentId: F_RULES,
    updatedAt: "2026-04-01T00:00:00Z",
    contentType: null,
  },
  "d-ikuji": {
    name: "育児介護休業規程.docx",
    parentId: F_RULES,
    updatedAt: "2026-04-01T00:00:00Z",
    contentType: null,
  },
  "d-guide": {
    name: "育休申請の手引き.md",
    parentId: F_KNOWLEDGE,
    updatedAt: "2026-09-12T00:00:00Z",
    contentType: "text/markdown",
  },
  "d-kyotei": {
    name: "2025年度_労使協定.pdf",
    parentId: F_AGREEMENT,
    updatedAt: "2025-03-28T00:00:00Z",
    contentType: "application/pdf",
  },
  "d-fukuri": {
    name: "福利厚生ガイド.md",
    parentId: F_KNOWLEDGE,
    updatedAt: "2026-06-30T00:00:00Z",
    contentType: "text/markdown",
  },
  "d-faq": {
    name: "勤怠FAQ.md",
    parentId: F_KNOWLEDGE,
    updatedAt: "2026-08-02T00:00:00Z",
    contentType: "text/markdown",
  },
};

const cite = (
  i: number,
  node_id: string,
  heading_path: string[],
  snippet: string,
  page: number | null = null,
): Citation => ({
  type: "citation",
  node_id,
  chunk_id: `chunk-${i}`,
  snippet,
  page,
  heading_path,
  score: 1 / i,
});

/// 到着順（[n] は n-1 番目）。
export const CITATIONS: Citation[] = [
  cite(
    1,
    "d-rule",
    ["第5章 休業", "第32条（育児休業）"],
    "従業員は、1歳に満たない子を養育するため必要があるときは、会社に申し出て育児休業をすることができる。ただし、有期雇用従業員にあっては、申出時点において、子が1歳6か月に達する日までに労働契約期間が満了し、更新されないことが明らかでない者に限る。",
  ),
  cite(
    2,
    "d-rule",
    ["第5章 休業", "第32条（育児休業）", "第3項"],
    "保育所等における保育の利用を希望し、申込みを行っているが、当面その実施が行われないときは、子が1歳6か月に達するまで育児休業をすることができる。さらに同様の事情があるときは、子が2歳に達するまで育児休業を延長することができる。",
  ),
  cite(
    3,
    "d-fukuri",
    ["慶弔・お祝い", "出産祝金"],
    "従業員または配偶者が出産したときは、出産祝金として 30,000 円を支給します。",
  ),
  cite(
    4,
    "d-rule",
    ["第5章 休業", "第33条（介護休業）"],
    "要介護状態にある家族を介護する従業員は、申し出により介護休業をすることができる。",
  ),
  cite(
    5,
    "d-ikuji",
    ["第2章 育児休業", "第4条（育児休業の申出の手続等）"],
    "育児休業をすることを希望する従業員は、原則として育児休業を開始しようとする日の1か月前までに、育児休業申出書を人事部労務担当に提出することにより申し出るものとする。",
  ),
  cite(
    6,
    "d-guide",
    ["申請の流れ", "提出期限"],
    "申出書は休業開始予定日の 1か月前 までに、Workday の「休業申請」から人事部へ提出してください。\n期限を過ぎた場合も申請はできますが、開始日が繰り下がることがあります。",
  ),
  cite(
    7,
    "d-kyotei",
    ["第2条（育児休業の適用除外）"],
    "会社は、次の従業員から育児休業の申出があったときは、その申出を拒むことができる。(1) 入社1年未満の従業員 (2) 申出の日から1年以内に雇用関係が終了することが明らかな従業員",
    4,
  ),
  cite(
    8,
    "d-faq",
    ["時短勤務"],
    "3歳に満たない子を養育する従業員は、1日の所定労働時間を6時間とする短時間勤務を申し出ることができます。",
  ),
];

export const ANSWER =
  "育児休業は、**子が1歳に達するまで**の間、性別を問わず取得できます[1]。保育所に入れないなどの事情があれば、**最長で2歳まで延長**できます[2]。\n\n" +
  "申請は **休業開始予定日の1か月前まで** に、育児休業申出書を人事部へ提出します[5][6]。なお、入社1年未満の従業員は労使協定により対象外となる場合があります[7]。";

/// 本文に引用マーカーが無い回答（古典 RAG 注入でモデルが番号を書かなかった場合）。
export const ANSWER_NO_MARKERS =
  "育児休業は子が1歳に達するまで取得でき、申請は開始予定日の1か月前までに行います。詳しくは就業規則と手引きを確認してください。";
