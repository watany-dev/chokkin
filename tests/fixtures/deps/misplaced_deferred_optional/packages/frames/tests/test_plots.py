def is_plotly_figure(value):
    return type(value).__module__.startswith("plotly")


def test_plot():
    assert not is_plotly_figure(None)
