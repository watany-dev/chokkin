import click
import httpx
import requests
import rich


def main() -> None:
    click.echo(rich.print, httpx.get, requests.get)
