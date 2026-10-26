from acme.registry import register_notification


@register_notification
class ToDoNotification:
    """Found through NOTIFICATIONS, never by name."""


class UnusedNotification:
    """Not registered."""
