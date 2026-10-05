import requests

try:
    import urllib3
except ImportError:
    urllib3 = None

try:
    import yaml
except ImportError:
    yaml = None


def main() -> None:
    requests.get('https://example.com')
    if urllib3 is not None:
        urllib3.PoolManager()
    if yaml is not None:
        yaml.safe_load('')
