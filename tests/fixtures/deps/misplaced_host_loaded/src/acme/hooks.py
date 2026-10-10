from _pytest.config import Config


def pytest_configure(config: Config) -> None:
    config.addinivalue_line("markers", "acme: acme tests")
