FROM python:3.12-slim
RUN pip install --no-cache-dir . \
    && alembic --help
CMD ["gunicorn", "acme.main:app", "--bind", "0.0.0.0:8000"]
