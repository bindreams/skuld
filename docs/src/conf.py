# Configuration file for the Sphinx documentation builder.
#
# For the full list of built-in configuration values, see the documentation:
# https://www.sphinx-doc.org/en/master/usage/configuration.html

from datetime import date

# -- Project information -----------------------------------------------------
# https://www.sphinx-doc.org/en/master/usage/configuration.html#project-information

project = "skuld"
author = "Anna Zhukova"
release = "0.1.0"
version = "0.1"

year = date.today().year
if year == 2025:
    copyright = f"2025, {author}"
else:
    copyright = f"2025-{year}, {author}"

html_title = f"{project} {release}"

# -- General configuration ---------------------------------------------------
# https://www.sphinx-doc.org/en/master/usage/configuration.html#general-configuration

extensions = [
    "myst_parser",
    "sphinx_design",
    "sphinx_copybutton",
]
myst_enable_extensions = [
    "colon_fence",
]

suppress_warnings = ["myst.header"]  # LICENSE.md has non-consecutive headers (Apache template)

templates_path = ["_templates"]
exclude_patterns = []

# -- Options for HTML output -------------------------------------------------
# https://www.sphinx-doc.org/en/master/usage/configuration.html#options-for-html-output

html_theme = "furo"
html_static_path = ["_static"]
