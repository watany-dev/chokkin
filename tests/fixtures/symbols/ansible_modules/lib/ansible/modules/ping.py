DOCUMENTATION = """
module: ping
"""

EXAMPLES = """
- ansible.builtin.ping:
"""

RETURN = """
ping:
  type: str
"""

UNUSED_CONSTANT = "pong"


def main() -> None:
    print("pong")


if __name__ == "__main__":
    main()
