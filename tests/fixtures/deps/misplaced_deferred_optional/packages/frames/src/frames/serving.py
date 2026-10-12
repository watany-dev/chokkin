def load_certificate(data):
    try:
        from cryptography import x509
    except ImportError as exc:
        raise ImportError("`pip install cryptography` to load certificates") from exc
    return x509.load_pem_x509_certificate(data)


def generate_key():
    from cryptography.hazmat.primitives.asymmetric import rsa

    return rsa.generate_private_key(public_exponent=65537, key_size=2048)
