import os

import yaml


def load_config():
    return yaml.safe_load(os.environ.get("ACME", ""))
