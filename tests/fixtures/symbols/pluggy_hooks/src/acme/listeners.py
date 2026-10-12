from acme.markers import hookimpl, pm


@hookimpl
def on_start() -> None:
    pass


@pm.hookspec(firstresult=True)
def on_stop() -> None:
    pass
