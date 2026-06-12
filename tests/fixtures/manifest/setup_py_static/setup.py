from setuptools import setup

setup(
    name="acme",
    version="0.1.0",
    install_requires=["requests", "flask>=1.0"],
    extras_require={"test": ["pytest"]},
)
