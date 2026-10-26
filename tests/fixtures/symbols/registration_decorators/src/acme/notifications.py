from acme import registry
from acme.hooks import permission
from acme.registry import register_notification, traced


@register_notification
class ToDoNotification:
    """Found through NOTIFICATIONS, never by name."""


@registry.notification
class LockNotification:
    """Registered by a decorator that does not say so in its name."""


@permission("unit.edit")
def check_unit_edit(user) -> bool:
    return True


@traced
def traced_but_dead() -> None:
    """Wrapped, not registered."""


class UnusedNotification:
    """Not registered."""
