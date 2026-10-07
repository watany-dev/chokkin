from typing import TYPE_CHECKING

if TYPE_CHECKING:
    import lazypkg


def first() -> None:
    import lazypkg

    lazypkg.run()


def second() -> None:
    from lazypkg import tool

    tool()


def main() -> None:
    from acme import types

    first()
    second()
    print(types)
