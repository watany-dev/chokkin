import requests
import urllib3


def main() -> None:
    requests.get('https://example.com')
    urllib3.PoolManager()
