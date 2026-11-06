"""conftest の見張りが効いていることを確かめる（手元の中継 8787 と外へは繋がない）。

わざと繋ぐ前に、見張りが掛かっていることと、宛先の判定が拒むことを先に確かめる（見張りが壊れていたときに、
このテスト自身が手元の中継へ繋がないため）。
"""

from __future__ import annotations

import socket

import httpx
import pytest

from netguard import BlockedConnection, allowed_address, guard_installed, take_blocked


def test_address_rules():
    assert not allowed_address(("127.0.0.1", 8787))
    assert not allowed_address(("::1", 8787, 0, 0))
    assert not allowed_address(("93.184.216.34", 443))
    assert not allowed_address(("api.anthropic.com", 443))
    assert allowed_address(("127.0.0.1", 18200))
    assert allowed_address(("::1", 18200, 0, 0))
    assert allowed_address("/run/some.sock")


def test_connecting_to_the_relay_raises():
    assert guard_installed()
    assert not allowed_address(("127.0.0.1", 8787))
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    try:
        with pytest.raises(BlockedConnection):
            s.connect(("127.0.0.1", 8787))
        with pytest.raises(BlockedConnection):
            s.connect_ex(("127.0.0.1", 8787))
    finally:
        s.close()
    with pytest.raises(BlockedConnection):
        socket.create_connection(("127.0.0.1", 8787), timeout=1)
    # httpx の経路でも、通信の誤り（ConnectError）ではなく見張りの例外になる。
    with pytest.raises(BlockedConnection):
        httpx.get("http://127.0.0.1:8787/spend", timeout=1)
    assert len(take_blocked()) >= 4


def test_outside_hosts_raise():
    assert guard_installed()
    with pytest.raises(BlockedConnection):
        socket.getaddrinfo("api.anthropic.com", 443)
    assert take_blocked()


def test_loopback_is_allowed():
    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.bind(("127.0.0.1", 0))
    srv.listen(1)
    try:
        c = socket.create_connection(srv.getsockname(), timeout=2)
        c.close()
    finally:
        srv.close()
