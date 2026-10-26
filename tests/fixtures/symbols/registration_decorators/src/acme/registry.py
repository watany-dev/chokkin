NOTIFICATIONS = []


def register_notification(cls):
    NOTIFICATIONS.append(cls)
    return cls
