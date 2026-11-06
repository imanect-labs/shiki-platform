"""単価と費用の計算（マイクロ USD・切り上げの整数・GAP-30 §6.1）。

- 段の境: 「入力＋キャッシュの読み＋キャッシュの書き」が閾値（既定 100,000 トークン）を超えたら high 段
  （手元の中継 `anthropic-relay.py` の `cost_of` と同じ）。
- 1 回分の費用 ＝ ⌈Σ トークン × 単価 ÷ 10^6⌉（単価はマイクロ USD／100 万トークン）。
- 最悪の予約（送る前の見積もり）: 入力は本文のバイト数を上限とし、出力は max_tokens。バイト数が閾値を超えたら
  high 段で数える。
"""

from __future__ import annotations

from dataclasses import dataclass

MICROS_PER_USD = 1_000_000
_PER_MILLION = 1_000_000


class UnknownModel(KeyError):
    """単価の無いモデル。"""


@dataclass(frozen=True)
class Tier:
    """1 段の単価（マイクロ USD／100 万トークン）。"""

    input: int
    output: int
    cache_read: int
    cache_write_5m: int
    cache_write_1h: int


@dataclass(frozen=True)
class ModelPrice:
    low: Tier
    high: Tier


@dataclass(frozen=True)
class Tokens:
    """Anthropic の usage から読んだトークン数。"""

    input: int = 0
    output: int = 0
    cache_read: int = 0
    cache_write_5m: int = 0
    cache_write_1h: int = 0

    @property
    def prompt(self) -> int:
        """段の判定に使うプロンプトの総数（入力＋キャッシュの読み＋キャッシュの書き）。"""
        return self.input + self.cache_read + self.cache_write_5m + self.cache_write_1h


def _int(v: object) -> int:
    if isinstance(v, bool) or not isinstance(v, int | float):
        return 0
    return max(int(v), 0)


def tokens_of(usage: dict) -> Tokens:
    """Anthropic の usage（`message_start`・`message_delta`・非ストリームの応答）からトークン数を読む。

    キャッシュの書きは `cache_creation.ephemeral_5m_input_tokens`／`ephemeral_1h_input_tokens` があれば
    それを、無ければ `cache_creation_input_tokens` を 5 分の書きとして数える（手元の中継と同じ）。
    """
    cc = usage.get("cache_creation") or {}
    if not isinstance(cc, dict):
        cc = {}
    cw5 = cc.get("ephemeral_5m_input_tokens")
    if cw5 is None:
        cw5 = usage.get("cache_creation_input_tokens", 0)
    return Tokens(
        input=_int(usage.get("input_tokens", 0)),
        output=_int(usage.get("output_tokens", 0)),
        cache_read=_int(usage.get("cache_read_input_tokens", 0)),
        cache_write_5m=_int(cw5),
        cache_write_1h=_int(cc.get("ephemeral_1h_input_tokens", 0)),
    )


def _ceil_div(num: int, den: int) -> int:
    return -(-num // den)


def usd_to_micros(usd: float) -> int:
    return round(usd * MICROS_PER_USD)


def micros_to_usd(micros: int) -> float:
    return micros / MICROS_PER_USD


@dataclass(frozen=True)
class Pricing:
    models: dict[str, ModelPrice]
    tier_threshold_tokens: int = 100_000

    def price(self, model: str) -> ModelPrice:
        try:
            return self.models[model]
        except KeyError:
            raise UnknownModel(model) from None

    def tier(self, model: str, prompt_tokens: int) -> Tier:
        p = self.price(model)
        return p.high if prompt_tokens > self.tier_threshold_tokens else p.low

    def cost_micros(self, model: str, usage: dict) -> int:
        """usage の実際の費用（マイクロ USD・切り上げ）。"""
        t = tokens_of(usage)
        tier = self.tier(model, t.prompt)
        num = (
            t.input * tier.input
            + t.output * tier.output
            + t.cache_read * tier.cache_read
            + t.cache_write_5m * tier.cache_write_5m
            + t.cache_write_1h * tier.cache_write_1h
        )
        return _ceil_div(num, _PER_MILLION)

    def reserve_micros(self, model: str, body_bytes: int, max_tokens: int) -> int:
        """送る前の最悪の費用（入力 ≦ 本文のバイト数・出力 ≦ max_tokens）。"""
        body_bytes = max(int(body_bytes), 0)
        max_tokens = max(int(max_tokens), 0)
        tier = self.tier(model, body_bytes)
        return _ceil_div(body_bytes * tier.input + max_tokens * tier.output, _PER_MILLION)
