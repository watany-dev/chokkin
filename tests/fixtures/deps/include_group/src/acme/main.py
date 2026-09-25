import httpx
import requests


def main() -> None:
    requests.get("https://example.com")
    httpx.get("https://example.com")
