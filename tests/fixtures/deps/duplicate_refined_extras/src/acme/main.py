import requests
import streamlit


def main() -> None:
    streamlit.write(requests.get("https://example.invalid").text)
