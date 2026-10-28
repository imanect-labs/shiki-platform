/// ホームの季節の添え書きに出す「〜の日」（#964）。
///
/// 毎日ある記念日をすべて載せるのではなく、出どころがはっきりしたもの（国民の祝日・五節句などの
/// 年中行事・法律で定められた日）だけを持ち、該当する日だけ出す。年で日付が動くもの（ハッピー
/// マンデー・春分/秋分・母の日など）はその年の日付を計算する。振替休日・国民の休日は出さない。

/// 月日が固定の日（月, 日, 名前）。
const FIXED: readonly (readonly [number, number, string])[] = [
  [1, 1, "元日"],
  [1, 7, "七草"],
  [2, 11, "建国記念の日"],
  [2, 14, "バレンタインデー"],
  [2, 23, "天皇誕生日"],
  [3, 3, "ひな祭り"],
  [3, 14, "ホワイトデー"],
  [4, 29, "昭和の日"],
  [5, 3, "憲法記念日"],
  [5, 4, "みどりの日"],
  [5, 5, "こどもの日"],
  [7, 7, "七夕"],
  [8, 11, "山の日"],
  [9, 1, "防災の日"],
  [9, 9, "重陽の節句"],
  [10, 27, "文字・活字文化の日"],
  [10, 31, "ハロウィン"],
  [11, 3, "文化の日"],
  [11, 15, "七五三"],
  [11, 23, "勤労感謝の日"],
  [12, 25, "クリスマス"],
  [12, 31, "大晦日"],
];

/// その月の第 n 週の曜日（0=日曜）の日付。
function nthWeekday(year: number, month: number, weekday: number, n: number): number {
  const first = new Date(year, month - 1, 1).getDay();
  return 1 + ((weekday - first + 7) % 7) + (n - 1) * 7;
}

/// 春分日・秋分日（1980〜2099 年に使われる近似式）。正式な日付は前年に官報で公表される暦要項で決まる。
function equinoxDay(year: number, base: number): number {
  const d = year - 1980;
  return Math.floor(base + 0.242194 * d - Math.floor(d / 4));
}

/// その年の、日付が動く日（月, 日, 名前）。
function movable(year: number): (readonly [number, number, string])[] {
  const days: (readonly [number, number, string])[] = [
    [1, nthWeekday(year, 1, 1, 2), "成人の日"],
    [5, nthWeekday(year, 5, 0, 2), "母の日"],
    [6, nthWeekday(year, 6, 0, 3), "父の日"],
    [7, nthWeekday(year, 7, 1, 3), "海の日"],
    [9, nthWeekday(year, 9, 1, 3), "敬老の日"],
    [10, nthWeekday(year, 10, 1, 2), "スポーツの日"],
  ];
  if (year >= 1980 && year <= 2099) {
    days.push([3, equinoxDay(year, 20.8431), "春分の日"]);
    days.push([9, equinoxDay(year, 23.2488), "秋分の日"]);
  }
  return days;
}

/// 与えた日（端末のローカル日付）が「〜の日」なら名前を返す。無ければ null。
export function notableDayOf(date: Date): string | null {
  const y = date.getFullYear();
  const m = date.getMonth() + 1;
  const d = date.getDate();
  const hit = [...FIXED, ...movable(y)].find(([mm, dd]) => mm === m && dd === d);
  return hit ? hit[2] : null;
}
