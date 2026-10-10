import pytest


class Plugin:
    @pytest.hookimpl
    def pytest_collection_modifyitems(self, items):
        items.reverse()
