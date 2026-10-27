import sys

import sniffio


def is_pyspark_session():
    return "pyspark" in sys.modules


# The predicate names `pyspark`, so only its imports are optional; a
# `sys.modules` check itself would gate `cloudpickle` too (#1272).
if is_pyspark_session():
    from pyspark.sql import DataFrame
    import cloudpickle


def create_event():
    if sniffio.current_async_library() == "trio":
        import trio

        return trio.Event()
    return None


if __name__ == "__main__":
    import rich

    rich.print(DataFrame, cloudpickle, create_event())
