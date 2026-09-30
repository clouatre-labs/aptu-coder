"""Tests for bench_v19.config wait-timeout ownership."""

import pytest
from bench_v18 import runner as v18
from bench_v19 import config


@pytest.fixture(autouse=True)
def _restore_v18_wait_timeout():
    saved = v18.SESSION_WAIT_TIMEOUT_S
    yield
    v18.SESSION_WAIT_TIMEOUT_S = saved


class TestApplyWaitTimeout:
    def test_default_sets_300(self, monkeypatch):
        monkeypatch.delenv("SESSION_WAIT_TIMEOUT_S", raising=False)
        config.apply_wait_timeout()
        assert v18.SESSION_WAIT_TIMEOUT_S == 300

    def test_env_override_applied(self, monkeypatch):
        monkeypatch.setenv("SESSION_WAIT_TIMEOUT_S", "900")
        config.apply_wait_timeout()
        assert v18.SESSION_WAIT_TIMEOUT_S == 900


class TestResolveWaitTimeoutS:
    def test_env_value_parsed_as_int(self, monkeypatch):
        monkeypatch.setenv("SESSION_WAIT_TIMEOUT_S", "42")
        assert config.resolve_wait_timeout_s() == 42

    def test_unset_falls_back_to_300(self, monkeypatch):
        monkeypatch.delenv("SESSION_WAIT_TIMEOUT_S", raising=False)
        assert config.resolve_wait_timeout_s() == 300

    def test_non_integer_fails_closed(self, monkeypatch):
        monkeypatch.setenv("SESSION_WAIT_TIMEOUT_S", "abc")
        with pytest.raises(ValueError):
            config.resolve_wait_timeout_s()
