import os

from setuptools import setup

here = os.path.abspath(os.path.dirname(__file__))
about = {}
with open(os.path.join(here, "src", "acme", "__version__.py")) as f:
    exec(f.read(), about)

setup(
    name=about["__title__"],
    version=about["__version__"],
    packages=["acme"],
    package_dir={"": "src"},
    install_requires=[],
)
