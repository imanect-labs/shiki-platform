/// 職員間メッセージ（Phase 14・design §4.14 / FR-18）の **UI モック用データ層**。
///
/// バックエンド（`crates/messaging`・OpenFGA `channel` 型・SSE 配信）は未実装で、
/// この画面はクライアント state だけで動く。将来 API に差し替えるとき同じ形が使えるよう、
/// 型は design §4.14 のドメイン（channel / message / reaction / read_state）と
/// content blocks（text / mention / file_ref）に寄せてある。
///
/// **時刻は固定文字列で持つ。** `new Date()` から組むと SSR とクライアントで値がずれて
/// hydration mismatch になるため、モックの発言は日付ラベルと時刻を直接持つ
/// （利用者がその場で送った発言だけ、クライアント側で現在時刻を採番する）。

/// 利用者（テナント内の principal）。外部ユーザーは参加できない（FR-18）。
export type Member = {
  id: string;
  name: string;
  /// アバターのフォールバック表示（姓の 1 文字）。
  initial: string;
  dept: string;
  /// 四季アクセントの巡回インデックス。アバターの色味に使う。
  seasonIndex: number;
  presence: "online" | "away" | "offline";
};

/// ドライブの文書。`file_ref` ブロックは **file_id だけ**を持ち、本文を複製しない。
/// 閲覧可否は文書側の ReBAC に従うため、ここでは「読める利用者の集合」で模す。
export type DriveFile = {
  id: string;
  name: string;
  kind: "sheet" | "doc" | "slide" | "pdf";
  /// ドライブ上の場所（表示用）。
  location: string;
  size: string;
  /// "all" は組織全員が読める。配列なら明示された利用者だけが読める。
  readableBy: "all" | string[];
};

/// 本文のブロック。§4.4 のチャットと型を分けない（text / mention / file_ref の部分集合）。
export type Block =
  | { kind: "text"; text: string }
  | { kind: "mention"; memberId: string }
  | { kind: "file_ref"; fileId: string };

export type Reaction = { emoji: string; by: string[] };

export type Message = {
  id: string;
  authorId: string;
  /// 日付の区切り見出し。連続する同ラベルはまとめて 1 本だけ描く。
  dayLabel: string;
  time: string;
  blocks: Block[];
  reactions: Reaction[];
  /// スレッド返信（`parent_id` で親にぶら下がる発言）。
  replies: Message[];
  edited?: boolean;
  /// シキ（AI）の回答。人の発言と視覚的に区別する。
  ai?: boolean;
  /// AI 回答が参照した文脈（表示用）。**照会者が読める発言と文書だけ**が並ぶ。
  sources?: string[];
  /// 生成中（タイプ表示のため本文が段階的に埋まる）。
  pending?: boolean;
};

export type ChannelKind = "public" | "private" | "dm";

export type Channel = {
  id: string;
  kind: ChannelKind;
  name: string;
  topic?: string;
  memberIds: string[];
  messages: Message[];
  /// 未読は `read_state.last_read_at` からの導出。ここでは「最初の未読発言 id」で模す
  /// （行ごとの既読フラグを持たない設計に合わせ、境界だけを保持する）。
  firstUnreadId?: string;
};

export const ME = "tanaka";

/// デモで「見る人」を切り替える候補。file_ref の権限差を実演するために使う。
export const VIEWER_CHOICES = ["tanaka", "suzuki"] as const;

export const MEMBERS: Member[] = [
  { id: "tanaka", name: "田中 誠", initial: "田", dept: "総務部", seasonIndex: 0, presence: "online" },
  { id: "sato", name: "佐藤 花子", initial: "佐", dept: "情報システム部", seasonIndex: 1, presence: "online" },
  { id: "suzuki", name: "鈴木 一郎", initial: "鈴", dept: "営業部", seasonIndex: 2, presence: "away" },
  { id: "yamada", name: "山田 美咲", initial: "山", dept: "人事部", seasonIndex: 3, presence: "offline" },
  { id: "kobayashi", name: "小林 健", initial: "小", dept: "総務部", seasonIndex: 1, presence: "online" },
  { id: "shiki", name: "Shiki アシスタント", initial: "S", dept: "AI アシスタント", seasonIndex: 0, presence: "online" },
];

export const FILES: DriveFile[] = [
  {
    id: "f-budget",
    name: "2027年度予算案.xlsx",
    kind: "sheet",
    location: "ドライブ / 総務部 / 予算",
    size: "184 KB",
    // 総務部と情シスの担当者だけに共有されている。営業部の鈴木は読めない。
    readableBy: ["tanaka", "sato", "kobayashi"],
  },
  {
    id: "f-rule",
    name: "就業規則_2027改定版.docx",
    kind: "doc",
    location: "ドライブ / 全社共有 / 規程",
    size: "96 KB",
    readableBy: "all",
  },
  {
    id: "f-report",
    name: "情シス定例_8月報告.pptx",
    kind: "slide",
    location: "ドライブ / 情報システム部",
    size: "2.4 MB",
    readableBy: "all",
  },
];

/// チャンネルの表示名。DM は自分以外の参加者名から組む（見る人によって変わる）。
export function channelTitle(channel: Channel, viewerId: string): string {
  if (channel.kind !== "dm") return channel.name;
  const others = channel.memberIds.filter((id) => id !== viewerId);
  if (others.length === 0) return "自分だけ";
  return others.map((id) => findMember(id).name).join("、");
}

export function findMember(id: string): Member {
  return MEMBERS.find((m) => m.id === id) ?? MEMBERS[0];
}

export function findFile(id: string): DriveFile | undefined {
  return FILES.find((f) => f.id === id);
}

/// その利用者が文書を読めるか。実装では StorageService 経由で ReBAC を評価する箇所。
export function canRead(file: DriveFile, viewerId: string): boolean {
  return file.readableBy === "all" || file.readableBy.includes(viewerId);
}

/// 発言を平文に落とす（検索・一覧のプレビュー用）。
export function plainText(msg: Message): string {
  return msg.blocks
    .map((b) => {
      if (b.kind === "text") return b.text;
      if (b.kind === "mention") return `@${findMember(b.memberId).name}`;
      return findFile(b.fileId)?.name ?? "添付";
    })
    .join(" ")
    .replace(/\s+/g, " ")
    .trim();
}

function text(t: string): Block {
  return { kind: "text", text: t };
}

let seq = 0;
function msg(
  authorId: string,
  dayLabel: string,
  time: string,
  blocks: Block[],
  extra: Partial<Message> = {},
): Message {
  seq += 1;
  return {
    id: `m${seq}`,
    authorId,
    dayLabel,
    time,
    blocks,
    reactions: [],
    replies: [],
    ...extra,
  };
}

export const CHANNELS: Channel[] = [
  {
    id: "ch-all",
    kind: "public",
    name: "全社お知らせ",
    topic: "全職員向けの周知。返信はスレッドでお願いします。",
    memberIds: ["tanaka", "sato", "suzuki", "yamada", "kobayashi"],
    firstUnreadId: "unread-all",
    messages: [
      msg("yamada", "8月20日", "09:12", [
        text("就業規則の改定版を掲載しました。10月1日施行です。"),
      ]),
      msg(
        "yamada",
        "8月20日",
        "09:13",
        [text("変更点は第3章と第7章です。"), { kind: "file_ref", fileId: "f-rule" }],
        {
          reactions: [
            { emoji: "👀", by: ["tanaka", "sato", "kobayashi"] },
            { emoji: "🙏", by: ["suzuki"] },
          ],
          replies: [
            msg("suzuki", "8月20日", "10:01", [
              text("第7章の適用開始は既存契約にも及びますか？"),
            ]),
            msg("yamada", "8月20日", "10:20", [
              { kind: "mention", memberId: "suzuki" },
              text(" 既存契約は経過措置があります。別途ご案内します。"),
            ]),
          ],
        },
      ),
      msg("kobayashi", "昨日", "16:40", [
        text("9月の防災訓練は9月5日(木) 14:00からです。各部署でご調整ください。"),
      ], {
        id: "unread-all",
        reactions: [{ emoji: "✅", by: ["sato", "yamada"] }],
      }),
      msg("sato", "今日", "08:55", [
        text("社内ネットワークの定期メンテナンスを今夜22時から実施します。"),
      ]),
    ],
  },
  {
    id: "ch-soumu",
    kind: "public",
    name: "総務-予算相談",
    topic: "2027年度予算の取りまとめ。資料はドライブ / 総務部 / 予算 に置いています。",
    memberIds: ["tanaka", "sato", "suzuki", "kobayashi"],
    messages: [
      msg("tanaka", "昨日", "17:30", [
        text("来期の予算案を各部から集め終えました。明日たたき台を共有します。"),
      ]),
      msg(
        "tanaka",
        "今日",
        "10:02",
        [
          text("お待たせしました。2027年度の予算案です。"),
          { kind: "file_ref", fileId: "f-budget" },
        ],
        {
          reactions: [{ emoji: "🎉", by: ["sato", "kobayashi"] }],
          replies: [
            msg("sato", "今日", "10:11", [
              text("情シスの枠、サーバ更改分を上乗せしてあります。ありがとうございます。"),
            ]),
            msg("kobayashi", "今日", "10:24", [
              text("備品費の内訳だけ、後で口頭で補足させてください。"),
            ]),
          ],
        },
      ),
      msg(
        "sato",
        "今日",
        "10:18",
        [
          { kind: "mention", memberId: "tanaka" },
          text(" 情シスの定例資料も併せて置いておきます。"),
          { kind: "file_ref", fileId: "f-report" },
        ],
        { reactions: [{ emoji: "👍", by: ["tanaka"] }] },
      ),
      msg("suzuki", "今日", "10:26", [
        text("営業部です。予算案の添付が開けないのですが、権限でしょうか？"),
      ]),
      msg("tanaka", "今日", "10:31", [
        { kind: "mention", memberId: "suzuki" },
        text(" 総務部内の共有にしていました。部長確認のうえ営業部にも共有します。"),
      ], { reactions: [{ emoji: "🙏", by: ["suzuki"] }] }),
    ],
  },
  {
    id: "ch-joushisu",
    kind: "public",
    name: "情シス-運用",
    topic: "障害・メンテナンスの共有。緊急は電話で。",
    memberIds: ["sato", "tanaka", "kobayashi"],
    messages: [
      msg("sato", "昨日", "11:05", [
        text("バックアップジョブの実行時間が伸びています。世代数を見直します。"),
      ]),
      msg("sato", "今日", "09:40", [
        text("見直し後、所要時間が 42 分 → 18 分になりました。"),
      ], { reactions: [{ emoji: "🚀", by: ["tanaka", "kobayashi"] }] }),
    ],
  },
  {
    id: "ch-private-budget",
    kind: "private",
    name: "予算査定-非公開",
    topic: "査定中の数字を扱います。参加者以外には見えません。",
    memberIds: ["tanaka", "kobayashi"],
    messages: [
      msg("kobayashi", "今日", "09:05", [
        text("人件費の見込みが前年比 +4.2% です。査定前に共有します。"),
      ]),
      msg("tanaka", "今日", "09:18", [
        text("了解です。役員会の前に総務内で握っておきましょう。"),
      ]),
    ],
  },
  {
    id: "dm-sato",
    kind: "dm",
    name: "佐藤 花子",
    memberIds: ["tanaka", "sato"],
    firstUnreadId: "unread-dm",
    messages: [
      msg("tanaka", "今日", "10:40", [text("予算案、情シス枠の根拠だけ後で教えてください。")]),
      msg("sato", "今日", "10:52", [
        text("サーバ更改の見積を添付します。3社比較の結果です。"),
      ], { id: "unread-dm" }),
      msg("sato", "今日", "10:53", [text("午後なら打ち合わせできます。15時どうでしょう。")]),
    ],
  },
  {
    id: "dm-suzuki",
    kind: "dm",
    name: "鈴木 一郎",
    memberIds: ["tanaka", "suzuki"],
    messages: [
      msg("suzuki", "8月20日", "14:22", [text("先ほどの件、ありがとうございました。")]),
    ],
  },
  {
    id: "dm-group",
    kind: "dm",
    name: "佐藤 花子, 山田 美咲",
    memberIds: ["tanaka", "sato", "yamada"],
    messages: [
      msg("yamada", "昨日", "13:10", [
        text("規程改定の周知文、総務と情シスで内容を揃えたいです。"),
      ]),
      msg("tanaka", "昨日", "13:25", [text("こちらで案を作ります。明日までにお送りします。")]),
    ],
  },
];

/// 発言検索。**検索者が参加しているチャンネルと自分あての DM に限る**（FR-18）。
/// 実装では pre-filter（参加チャンネルへの絞り込み）＋ post-filter（結果の channel 再評価）の
/// 二段 authz になる。ここでは memberIds への所属だけで模す。
export type SearchHit = { channel: Channel; message: Message; parent?: Message };

export function searchMessages(
  channels: Channel[],
  query: string,
  viewerId: string,
): SearchHit[] {
  const q = query.trim();
  if (!q) return [];
  const hits: SearchHit[] = [];
  for (const channel of channels) {
    // pre-filter: 参加していないチャンネルは走査対象にすら入れない。
    if (!channel.memberIds.includes(viewerId)) continue;
    for (const m of channel.messages) {
      if (plainText(m).includes(q)) hits.push({ channel, message: m });
      for (const r of m.replies) {
        if (plainText(r).includes(q)) hits.push({ channel, message: r, parent: m });
      }
    }
  }
  return hits;
}

/// 未読件数（`firstUnreadId` 以降の件数として導出する）。
export function unreadCount(channel: Channel): number {
  if (!channel.firstUnreadId) return 0;
  const i = channel.messages.findIndex((m) => m.id === channel.firstUnreadId);
  return i < 0 ? 0 : channel.messages.length - i;
}
