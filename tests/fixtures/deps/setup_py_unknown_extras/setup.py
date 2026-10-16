import json
import pathlib

from setuptools import setup

setup(
    name="acme",
    install_requires=["requests"],
    extras_require=json.loads(pathlib.Path("extras.json").read_text()),
)
