from acme.web import app


@app.route("/")
def index():
    return "ok"
