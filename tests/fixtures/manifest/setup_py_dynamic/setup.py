from setuptools import setup
import pathlib

setup(
    name="dynamic",
    version="0.1.0",
    install_requires=pathlib.Path("requirements.txt").read_text().splitlines(),
)
