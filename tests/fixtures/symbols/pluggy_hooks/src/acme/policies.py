import pluggy

local_settings_hookspec = pluggy.HookspecMarker("acme.policy")


@local_settings_hookspec
def task_policy(task) -> None:
    """Implemented by name in a user's policy plugin."""


def helper() -> None:
    """Not a hook."""
