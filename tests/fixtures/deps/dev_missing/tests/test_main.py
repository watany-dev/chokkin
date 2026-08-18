import yaml


def test_main() -> None:
    assert yaml.safe_load("enabled: true") == {"enabled": True}
