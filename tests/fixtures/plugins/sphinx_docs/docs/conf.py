project = "sphinx-docs"
extensions = [
    "sphinx.ext.autodoc",
    "myst_parser",
]
extensions.append("sphinx_copybutton")
extensions.extend(["sphinx.ext.napoleon"])
extensions += ["sphinx_design"]
