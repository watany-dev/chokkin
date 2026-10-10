import hypothesis
import pytest


@pytest.fixture
def acme_settings():
    return hypothesis.settings()
