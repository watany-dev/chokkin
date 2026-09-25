try:
    import yaml
except ImportError:
    yaml = None


def main() -> None:
    if yaml is not None:
        yaml.safe_load("enabled: true")
