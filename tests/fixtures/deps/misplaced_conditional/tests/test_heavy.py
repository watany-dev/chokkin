from acme import heavy, optional_io


def test_heavy():
    assert heavy.compute() and optional_io.read()
