# Security Policy

## Reporting a vulnerability

Please **do not** open a public GitHub issue for security vulnerabilities.
Use [GitHub private security advisories](https://github.com/watany-dev/yokei/security/advisories/new)
to report them privately.

**Response timeline:**

| Stage | Target |
|---|---|
| Acknowledgement | 3 business days |
| Severity assessment | 10 days |
| Patch / advisory | 90 days (expedited for high/critical) |

## Scope

This policy covers the `yokei` binary distributed on PyPI (Linux/macOS/Windows
wheels), its public library API, and the configuration parser.

**Out of scope:**

- Pre-release versions (alpha/beta/rc tags).
- Issues that require an attacker to already have equivalent shell access.
- Behavior of the analyzed project's own code (yokei performs static analysis
  only and never executes it).

## Supported versions

Until a stable 1.0.0 release, only the latest published version is supported.
