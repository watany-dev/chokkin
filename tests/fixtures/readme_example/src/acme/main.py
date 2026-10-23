from fastapi import FastAPI

from acme.auth import TokenBackend
from acme.config import load_config
from acme.utils import slugify

if __name__ == "__main__":
    load_config()
    FastAPI(title=slugify("acme"), dependencies=[TokenBackend])
