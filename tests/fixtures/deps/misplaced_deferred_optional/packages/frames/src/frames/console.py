import sys

if sys.platform != "win32":
    import uvloop

if sys.platform == "win32":
    try:
        import colorama
    except ImportError:
        colorama = None


def run(main):
    import uvloop

    return uvloop.run(main)


def init_colors():
    import colorama

    colorama.just_fix_windows_console()
