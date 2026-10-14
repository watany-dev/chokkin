#!/bin/sh
echo "starting worker"
celery -A worker.celery:app worker --loglevel=info
