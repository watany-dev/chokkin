import pathlib

from setuptools import setup

setup(name="acme", install_requires=pathlib.Path("deps.cfg").read_text().splitlines())
