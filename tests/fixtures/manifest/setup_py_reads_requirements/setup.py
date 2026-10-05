from setuptools import setup


def reqs(path):
    return open(path).read().splitlines()


setup(name="acme", install_requires=reqs("requirements.txt"))
