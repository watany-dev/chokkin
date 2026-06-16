from celery import shared_task


@shared_task
def refresh_index():
    return None
