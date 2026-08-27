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
  /// アバターのフォールバック表示（姓の 1 文字）。写真が読めないときに出る。
  initial: string;
  /// 顔写真（`web/public` 配下）。デモ用の素材で、実在の人物ではない。
  photo?: string;
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

/// 本文のブロック。**この型はモック限定**で、実装時は codegen（Rust → OpenAPI → TS）から出す。
///
/// 生成物の `ContentBlock`（`web/src/generated/api.d.ts`）は §4.4 のチャット用で、
/// `file_ref` が `name` をブロック自身に持つ。§4.14 は messaging の `file_ref` について
/// 「**file_id だけを持ち本文を複製しない**」と定めるため、ここでは `name` を持たせない
/// （持たせると、権限の無い相手にもファイル名がブロックごと届く）。
/// `mention` は現行の生成 union に無い。実装時は messaging 用のブロックを Rust 側に足し、
/// この手書き型を捨てる。内部タグ（`type`）と snake_case は生成物に合わせてある。
export type MessageBlock =
  | { type: "text"; text: string }
  | { type: "mention"; member_id: string }
  | { type: "file_ref"; node_id: string };

export type Reaction = { emoji: string; by: string[] };

export type Message = {
  id: string;
  authorId: string;
  /// 日付の区切り見出し。連続する同ラベルはまとめて 1 本だけ描く。
  dayLabel: string;
  time: string;
  blocks: MessageBlock[];
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

/// 顔写真はすべて **生成モデルが作った実在しない人物**（`web/public/demo/avatars/`）。
/// デモ用の仮素材なので、実運用では利用者のプロフィール画像に差し替える。
const PHOTO = (id: string) => `/demo/avatars/${id}.jpg`;

export const MEMBERS: Member[] = [
  { id: "tanaka", name: "田中 誠", initial: "田", dept: "総務部", seasonIndex: 0, presence: "online", photo: PHOTO("tanaka") },
  { id: "sato", name: "佐藤 花子", initial: "佐", dept: "情報システム部", seasonIndex: 1, presence: "online", photo: PHOTO("sato") },
  { id: "suzuki", name: "鈴木 一郎", initial: "鈴", dept: "営業部", seasonIndex: 2, presence: "away", photo: PHOTO("suzuki") },
  { id: "yamada", name: "山田 美咲", initial: "山", dept: "人事部", seasonIndex: 3, presence: "offline", photo: PHOTO("yamada") },
  { id: "kobayashi", name: "小林 健", initial: "小", dept: "総務部", seasonIndex: 1, presence: "online", photo: PHOTO("kobayashi") },
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

/// 閲覧者から見た添付の表示情報。**読めない相手にはメタデータを一切返さない。**
/// 実装では StorageService 経由の ReBAC 評価がサーバ側でこれを返す。
/// `readableBy` のような ACL をレンダラへ渡さないための境界。
export type FileRefView =
  | { readable: true; name: string; kind: DriveFile["kind"]; location: string; size: string }
  | { readable: false };

export function fileRefView(nodeId: string, viewerId: string): FileRefView | null {
  const file = findFile(nodeId);
  if (!file) return null;
  if (!canRead(file, viewerId)) return { readable: false };
  return {
    readable: true,
    name: file.name,
    kind: file.kind,
    location: file.location,
    size: file.size,
  };
}

/// 発言を平文に落とす（検索・一覧のプレビュー用）。
///
/// **添付は閲覧者の権限を見る。** ここで無条件にファイル名を返すと、本文側で
/// 「参照できない添付」として伏せた名前が検索プレビュー経由で漏れる。
export function plainText(msg: Message, viewerId: string): string {
  return msg.blocks
    .map((b) => {
      if (b.type === "text") return b.text;
      if (b.type === "mention") return `@${findMember(b.member_id).name}`;
      const file = findFile(b.node_id);
      if (!file || !canRead(file, viewerId)) return "［参照できない添付］";
      return file.name;
    })
    .join(" ")
    .replace(/\s+/g, " ")
    .trim();
}

function text(t: string): MessageBlock {
  return { type: "text", text: t };
}

let seq = 0;
function msg(
  authorId: string,
  dayLabel: string,
  time: string,
  blocks: MessageBlock[],
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
        [text("変更点は第3章と第7章です。"), { type: "file_ref", node_id: "f-rule" }],
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
              { type: "mention", member_id: "suzuki" },
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
    memberIds: ["tanaka", "sato", "suzuki", "kobayashi", "yamada"],
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
          { type: "file_ref", node_id: "f-budget" },
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
          { type: "mention", member_id: "tanaka" },
          text(" 情シスの定例資料も併せて置いておきます。"),
          { type: "file_ref", node_id: "f-report" },
        ],
        { reactions: [{ emoji: "👍", by: ["tanaka"] }] },
      ),
      msg("suzuki", "今日", "10:26", [
        text("営業部です。予算案の添付が開けないのですが、権限でしょうか？"),
      ]),
      msg("tanaka", "今日", "10:31", [
        { type: "mention", member_id: "suzuki" },
        text(" 総務部内の共有にしていました。部長確認のうえ営業部にも共有します。"),
      ], { reactions: [{ emoji: "🙏", by: ["suzuki"] }] }),
    ],
  },
  {
    id: "ch-joushisu",
    kind: "public",
    name: "情シス-運用",
    topic: "障害・メンテナンスの共有。緊急は電話で。",
    // 公開チャンネルは組織の全員が読める（FR-18）。参加を明示タプルで表すか org 継承にするかは
    // 14.2 のポリシ決定（human 承認待ち）だが、「全員が読める」という結果はどちらでも同じ。
    memberIds: ["sato", "tanaka", "kobayashi", "suzuki", "yamada"],
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
      if (plainText(m, viewerId).includes(q)) hits.push({ channel, message: m });
      for (const r of m.replies) {
        if (plainText(r, viewerId).includes(q)) hits.push({ channel, message: r, parent: m });
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
