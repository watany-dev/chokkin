from frames import convert, plots, serving


def main():
    return convert, plots, serving


def live_browser():
    from frames.testing import browser

    return browser.browser()
