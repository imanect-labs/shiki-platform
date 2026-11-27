from __future__ import annotations

import pytest

from abeval.config import load_config
from abeval.pricing import Pricing, UnknownModel, micros_to_usd, tokens_of, usd_to_micros

MODEL = "claude-haiku-5-5"


@pytest.fixture
def pricing() -> Pricing:
    return load_config().pricing


def test_values_match_the_design(pricing):
    p = pricing.price(MODEL)
    assert (p.low.input, p.low.output) == (100_000, 500_000)
    assert (p.low.cache_read, p.low.cache_write_5m, p.low.cache_write_1h) == (
        10_000,
        125_000,
        200_000,
    )
    # high 段は low 段の 5 倍。
    for f in ("input", "output", "cache_read", "cache_write_5m", "cache_write_1h"):
        assert getattr(p.high, f) == 5 * getattr(p.low, f), f
    assert pricing.tier_threshold_tokens == 100_000


def test_tier_boundary_is_strictly_greater(pricing):
    p = pricing.price(MODEL)
    assert pricing.tier(MODEL, 100_000) == p.low
    assert pricing.tier(MODEL, 100_001) == p.high
    assert pricing.cost_micros(MODEL, {"input_tokens": 100_000}) == 10_000
    assert pricing.cost_micros(MODEL, {"input_tokens": 100_001}) == 50_001


def test_cache_tokens_count_toward_the_tier(pricing):
    # 入力 60,000 ＋ キャッシュの読み 40,001 ＝ 100,001 → high 段。
    usage = {"input_tokens": 60_000, "cache_read_input_tokens": 40_001}
    expected = -(-(60_000 * 500_000 + 40_001 * 50_000) // 1_000_000)
    assert pricing.cost_micros(MODEL, usage) == expected
    # キャッシュの書き（5 分）も入る。
    usage = {"input_tokens": 60_000, "cache_creation_input_tokens": 40_001}
    expected = -(-(60_000 * 500_000 + 40_001 * 625_000) // 1_000_000)
    assert pricing.cost_micros(MODEL, usage) == expected
    # 1 時間の書きの内訳があれば、そちらで数える。
    usage = {
        "input_tokens": 99_000,
        "cache_creation_input_tokens": 2_000,
        "cache_creation": {"ephemeral_5m_input_tokens": 500, "ephemeral_1h_input_tokens": 1_500},
    }
    t = tokens_of(usage)
    assert (t.cache_write_5m, t.cache_write_1h, t.prompt) == (500, 1_500, 101_000)
    expected = -(-(99_000 * 500_000 + 500 * 625_000 + 1_500 * 1_000_000) // 1_000_000)
    assert pricing.cost_micros(MODEL, usage) == expected


def test_cost_rounds_up_to_a_whole_micro(pricing):
    # 1 トークン × $0.10/M ＝ 0.1 マイクロ → 切り上げて 1。
    assert pricing.cost_micros(MODEL, {"input_tokens": 1}) == 1
    assert pricing.cost_micros(MODEL, {}) == 0
    # 出力 1 トークン × $0.50/M ＝ 0.5 → 1。10 トークン ＝ 5。
    assert pricing.cost_micros(MODEL, {"output_tokens": 1}) == 1
    assert pricing.cost_micros(MODEL, {"output_tokens": 10}) == 5


def test_reserve_uses_body_bytes_and_max_tokens(pricing):
    # 1,000 バイト × 100,000 ＋ 8,192 × 500,000 → ⌈(1e8 + 4.096e9) / 1e6⌉ = 4196。
    assert pricing.reserve_micros(MODEL, 1_000, 8_192) == 4_196
    # バイト数が 100K を超えたら high 段で数える。
    low = pricing.reserve_micros(MODEL, 100_000, 8_192)
    high = pricing.reserve_micros(MODEL, 100_001, 8_192)
    assert low == 10_000 + 4_096
    assert high == -(-(100_001 * 500_000 + 8_192 * 2_500_000) // 1_000_000)
    assert high > 5 * low - 10


def test_unknown_model_is_an_error(pricing):
    with pytest.raises(UnknownModel):
        pricing.cost_micros("claude-opus-5-5", {"input_tokens": 1})
    with pytest.raises(UnknownModel):
        pricing.reserve_micros("gpt-x", 1, 1)


def test_bad_usage_values_count_as_zero(pricing):
    assert pricing.cost_micros(MODEL, {"input_tokens": None, "output_tokens": True}) == 0
    assert pricing.cost_micros(MODEL, {"input_tokens": -5}) == 0


def test_usd_micros_roundtrip():
    assert usd_to_micros(35) == 35_000_000
    assert usd_to_micros(0.000001) == 1
    assert micros_to_usd(10_000_000) == 10.0
