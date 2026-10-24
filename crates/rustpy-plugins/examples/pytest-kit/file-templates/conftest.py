"""Shared pytest fixtures for ${PROJECT_NAME} (created ${DATE})."""

import pytest


@pytest.fixture
def tmp_project(tmp_path):
    """An empty project directory for a test."""
    return tmp_path
