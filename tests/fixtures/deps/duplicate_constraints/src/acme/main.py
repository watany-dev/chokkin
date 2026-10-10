import anyio
import click
import httpx
import requests
import rich


def main() -> None:
    click.echo(anyio.run, rich.print, httpx.get, requests.get)
