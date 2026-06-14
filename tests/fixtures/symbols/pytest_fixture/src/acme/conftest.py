import pytest


@pytest.fixture
def sample_data() -> dict[str, str]:
    return {"key": "value"}
