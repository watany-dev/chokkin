#!/bin/sh
flask --app src/web/missing.py run
flask --app ../elsewhere/app.py run
flask --app "$APP_DIR/app.py" run
