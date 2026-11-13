/// 引用表示ギャラリーの固定フィクスチャ（issue #505）。
///
/// 想定: 「育休の取得条件と申請期限を教えて」に doc_search を 2 回呼んで答えた。
/// 引用は到着順に 8 件（1 回目 4 件・2 回目 4 件）。本文はそのうち 1,2,5,6,7 を引用し、
/// 3,4,8 は検索したが使わなかった。番号は応答内の通し番号（サーバ側の採番修正後の姿）。
import type { Citation, HelpCitation } from "@/lib/chat-api";
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

/// 社内文書と Web の出典が混ざった回答（#891）。
///
/// 想定: doc_search（1, 2）→ web_search（3〜5）→ 3 を web_fetch した。取得したページは検索結果と
/// 同じ番号（3）を使い回す。本文は 1, 3, 4 を引用し、2, 5 は使わなかった。
const web = (cite_id: number, url: string, title: string, snippet: string): Citation => ({
  type: "citation",
  cite_id,
  node_id: "",
  chunk_id: `web:${url}`,
  snippet,
  heading_path: [],
  score: 0,
  web: { url, title },
});

export const MIXED_CITATIONS: Citation[] = [
  { ...CITATIONS[0], cite_id: 1 },
  { ...CITATIONS[4], cite_id: 2 },
  web(
    3,
    "https://www.mhlw.go.jp/stf/seisakunitsuite/bunya/0000130583.html",
    "育児・介護休業法について",
    "育児・介護休業法の改正により、2025年4月1日から、子の看護休暇の対象が小学校3年生修了までに拡大されました。",
  ),
  web(
    4,
    "https://example.com/news/2025/childcare-leave",
    "男性の育休取得率、過去最高の 40% に",
    "厚生労働省の調査によると、2024 年度の男性の育児休業取得率は 40.5% となり、前年度から 10 ポイント上昇した。",
  ),
  web(5, "https://example.org/guide/ikukyu", "", "育休の申請に必要な書類と提出先をまとめました。"),
];

export const MIXED_ANSWER =
  "社内の規程では、子が1歳に達するまで育児休業を取得できます[1]。\n\n" +
  "法改正により、2025年4月から子の看護休暇の対象が小学校3年生修了までに広がりました[3]。" +
  "男性の育休取得率は 2024 年度に 40.5% と過去最高になっています[4]。";

/// 製品ヘルプの出典（`help_search`・`[Hn]`）。社内文書とは番号の台帳が別。
export const HELP_CITATIONS: HelpCitation[] = [
  {
    type: "help_citation",
    cite_id: 1,
    corpus_version: "b1d9b53e934f",
    chunk_id: "help-1",
    title: "パスワードを設定・変更する",
    heading_path: ["パスワードを設定・変更する", "手順"],
    snippet:
      "右側の「パスワード」の欄で「パスワードを変更」（①）を押します。本人確認と設定の画面へ移動します。未設定なら「パスワードを設定」を押します。",
    url: "https://shiki.imanect.app/help/account/security/password/#%E6%89%8B%E9%A0%86",
  },
  {
    type: "help_citation",
    cite_id: 2,
    corpus_version: "b1d9b53e934f",
    chunk_id: "help-2",
    title: "パスワードを設定・変更する",
    heading_path: ["パスワードを設定・変更する", "始める前に"],
    snippet:
      "個人設定の左側で「ログイン方法」を押します。会社のアカウントでログインする SSO を利用している場合は、組織のパスワード変更の案内を確認してください。",
    url: "https://shiki.imanect.app/help/account/security/password/#%E5%A7%8B%E3%82%81%E3%82%8B%E5%89%8D%E3%81%AB",
  },
  {
    type: "help_citation",
    cite_id: 3,
    corpus_version: "b1d9b53e934f",
    chunk_id: "help-3",
    title: "パスワードでログインする",
    heading_path: ["パスワードでログインする", "手順"],
    snippet: "ログイン画面でユーザー名とパスワードを入力し、「ログイン」を押します。",
    url: "https://shiki.imanect.app/help/getting-started/sign-in/password/#%E6%89%8B%E9%A0%86",
  },
];

export const HELP_ANSWER =
  "Shiki でパスワードを変更する手順は次のとおりです。\n\n" +
  "1. 個人設定の左側で「ログイン方法」を押します。[H2]\n" +
  "2. 右側の「パスワード」欄で「パスワードを変更」を押します。[H1]\n" +
  "3. 次回からは新しいパスワードでログインします。[H3]\n\n" +
  "会社のアカウント（SSO）でログインしている場合は、組織の案内に従ってください。[H2]";

/// 社内文書の出典（CITATIONS）とヘルプの出典が隣り合う回答（並び順の両方と、コード中の番号）。
export const HELP_MIXED_ANSWER =
  "育休の申請は社内の規程に従い、Shiki のワークフローから出します。[H1][1]\n\n" +
  "申請期限は開始の 1 か月前までです。[2][H3]\n\n" +
  "コード中の `[H1][1]` は変えません。";
