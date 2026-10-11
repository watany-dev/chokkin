def is_sympy_expression(body):
    return hasattr(body, "free_symbols")


def latex(body):
    if is_sympy_expression(body):
        import sympy

        return sympy.latex(body)
    return body
