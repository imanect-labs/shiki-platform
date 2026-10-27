/// 「AI で判定」の質問・選択肢のキーを 1 文字ずつ打ち変えるときの付け替え（エッジ・振り分け）。描画を持たない。
///
/// 答えで分けている質問の選択肢のキーは出口の名前になる（導出の正は Rust の `AiDecideParams::ports`・fixture は
/// crates/workflow-engine/tests/fixtures/port_cases.json）。ここで写しているのは「選択肢の出口の名前＝その選択肢の
/// キー」という 1 点だけで、出口の集合は検証 API の `node_ports` で描く（H15）。
///
/// 打っている途中でほかの選択肢と同じキーを経由すると、名前だけでは誰のエッジか分からなくなる（重なった次の 1 手で、
/// ほかの選択肢のエッジを自分の出口へ移してしまう）。そこで、重なっている間は付け替えず、その選択肢のエッジが今
/// どの出口にあるか（`held`）を覚えておき、重なりが解けたらそこから付け替える。質問のキーと振り分け（`route.question`）
/// も同じで、重なっている間はどの質問の振り分けかを覚えておく。

/// 選択肢のキーを変えた 1 手の付け替え。`move` があれば、その出口から出るエッジを付け替える。
export type OptionRenamePlan = {
  move: { from: string; to: string } | null;
  /// この選択肢のエッジが残っている出口（重なりが解けるまで付け替えを止めた・無ければ undefined）。
  held: string | undefined;
};

/// 選択肢 `index` のキーを `prevKeys[index]` から `to` へ変えたときの付け替え。
///
/// - `held` は前の手で付け替えを止めたときの出口。今もエッジがあり、ほかの選択肢のキーでなければ、この選択肢の
///   エッジはそこにある（無ければ前のキーの出口）。
/// - 付け替えない: エッジが無い・その出口がほかの選択肢の名前でもある（どちらのエッジか分からない）。
/// - 待つ（`held` を返す）: 新しいキーがほかの選択肢のキーか、もうエッジの出ている出口（線を混ぜない）。
export function planOptionRename(input: {
  prevKeys: readonly string[];
  index: number;
  to: string;
  held: string | undefined;
  edgePorts: readonly string[];
}): OptionRenamePlan {
  const { prevKeys, index, to, held, edgePorts } = input;
  const from = prevKeys[index] ?? "";
  const others = prevKeys.filter((_, j) => j !== index);
  const at = held !== undefined && edgePorts.includes(held) && !others.includes(held) ? held : from;
  if (!edgePorts.includes(at) || others.includes(at) || at === to) return { move: null, held: undefined };
  if (others.includes(to) || edgePorts.includes(to)) return { move: null, held: at };
  return { move: { from: at, to }, held: undefined };
}

/// 質問のキーを変えた 1 手の振り分けの付け替え。`question` があれば `route.question` をそれにする。
export type RouteRenamePlan = {
  question: string | null;
  /// 振り分けを持つ質問の位置（ほかの質問と同じキーを通っている間だけ覚える・無ければ undefined）。
  held: number | undefined;
};

/// 質問 `index` のキーを `prevKeys[index]` から `to` へ変えたときの振り分け（`route` は今の `route.question`）。
///
/// 振り分けを持つのは、前の手で覚えた質問（今もそのキーが振り分けの質問と同じなら）か、振り分けの質問と同じキーの
/// ただ 1 つの質問。ほかの質問のキーを打ち変えて振り分けの質問のキーを通っても、振り分けは動かない。
export function planRouteRename(input: {
  prevKeys: readonly string[];
  index: number;
  to: string;
  route: string;
  held: number | undefined;
}): RouteRenamePlan {
  const { prevKeys, index, to, route, held } = input;
  const heldOk = held !== undefined && prevKeys[held] === route;
  const matches = prevKeys.flatMap((k, j) => (k === route ? [j] : []));
  const owner = heldOk ? held : matches.length === 1 ? matches[0] : -1;
  if (owner !== index) return { question: null, held: heldOk ? held : undefined };
  const others = prevKeys.filter((_, j) => j !== index);
  return { question: to, held: others.includes(to) ? index : undefined };
}

/// 付け替えの記憶を捨てるべき変更か（質問・選択肢の数・並び・キーが、付け替え以外で変わった）。
export function renameShape(questions: readonly { key: string; options?: readonly { key: string }[] }[]): string {
  return JSON.stringify(questions.map((q) => [q.key, (q.options ?? []).map((o) => o.key)]));
}
