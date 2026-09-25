PYTHON ?= python
SERVER := gunicorn
include extra.mk

.PHONY: migrate worker serve report

migrate:
	@uv run alembic upgrade head

worker:
	-python -m celery -A acme.main \
		worker --loglevel=info

serve:
	$(SERVER) acme.main:app
	$(PYTHON) -m gunicorn acme.main:app

report:
	echo $(shell coverage report) \
		gunicorn
