"""Tests for ${NAME} (created ${DATE} by ${USER})."""

import pytest


@pytest.fixture
def subject():
    return None


def test_subject_is_empty(subject):
    assert subject is None
