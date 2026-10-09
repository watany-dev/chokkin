import sys

import sniffio

if "pyspark" in sys.modules:
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
