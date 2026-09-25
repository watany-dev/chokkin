FROM python:3.12-slim
ENTRYPOINT ["celery", "-A", "acme.main", "worker"]
