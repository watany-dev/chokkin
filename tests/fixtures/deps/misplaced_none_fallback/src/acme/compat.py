try:
    import markdown
except ImportError:
    markdown = None

try:
    import pygments
except ImportError:
    pygments = None

try:
    import yaml
except ImportError:
    yaml = None

if markdown is not None and pygments is not None:
    from markdown.preprocessors import Preprocessor
    from pygments.lexers import TextLexer

if yaml is None:
    from yaml import nodes


def main():
    return Preprocessor, TextLexer, nodes
