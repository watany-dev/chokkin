import functools

NOTIFICATIONS = []
PERMISSIONS = {}


def register_notification(cls):
    NOTIFICATIONS.append(cls)
    return cls


def notification(cls):
    NOTIFICATIONS.append(cls)
    return cls


def permission(*names):
    def wrap(function):
        for name in names:
            PERMISSIONS[name] = function
        return function

    return wrap


def traced(function):
    @functools.wraps(function)
    def inner(*args):
        return function(*args)

    return inner
