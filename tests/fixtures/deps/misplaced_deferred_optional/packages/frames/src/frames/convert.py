def is_polars_dataframe(obj):
    return type(obj).__module__.startswith("polars")


def is_openai_v1():
    return False


def to_polars(obj):
    import polars

    return polars.from_dict(obj)
