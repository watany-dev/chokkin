import sniffio


def sleep_forever():
    library = sniffio.current_async_library()
    if library == "trio":
        import trio

        return trio.sleep_forever()
    return None


def spawn(force_curio):
    library = sniffio.current_async_library()
    if force_curio:
        library = "curio"
    if library == "curio":
        import curio

        return curio.spawn
    return None
