from celery import Celery

instance = Celery("worker")
