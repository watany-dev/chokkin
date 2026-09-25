INSTALLED_APPS = [
    "django.contrib.admin",
    "django.contrib.auth",
    "myapp",
]

MIDDLEWARE = [
    "django.middleware.security.SecurityMiddleware",
]

ROOT_URLCONF = "mysite.urls"
WSGI_APPLICATION = "mysite.wsgi:application"
