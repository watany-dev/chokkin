import nox


@nox.session
def tests(session):
    session.run("pytest")
