import pytest
hypothesis = pytest.importorskip('hypothesis')
from hypothesis import given, strategies


@given(strategies.integers(min_value=0, max_value=10))
def test_falsifying_example(value):
    assert value != 0
