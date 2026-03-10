---
name: security-best-practices
description: Secure development practices covering OWASP top risks and common vulnerabilities
version: "1.0"
tags: [security, owasp, best-practices]
---

# Security Best Practices

## Input Validation

- Never trust user input. Validate at system boundaries (API endpoints, form handlers, CLI args).
- Use allowlists over denylists when possible.
- Validate type, length, range, and format.
- Sanitize output based on context (HTML, SQL, shell, URL).

## Injection Prevention

- **SQL**: Use parameterized queries / prepared statements. Never concatenate user input into SQL strings.
- **Command injection**: Avoid passing user input to shell commands. If unavoidable, use safe APIs that don't invoke a shell (e.g., subprocess with list args, not string).
- **XSS**: Escape output in HTML contexts. Use framework auto-escaping. Set Content-Security-Policy headers.
- **Path traversal**: Resolve paths and verify they stay within expected directories. Reject `..` sequences.

## Secrets Management

- Never commit secrets to version control (API keys, passwords, tokens).
- Use environment variables or a secrets manager.
- Add `.env` files and credential patterns to `.gitignore`.
- Rotate secrets if they're ever exposed, even briefly.

## Authentication and Authorization

- Hash passwords with bcrypt, scrypt, or argon2. Never use MD5 or SHA for passwords.
- Use short-lived tokens (JWT, session tokens) with proper expiry.
- Check authorization on every request, not just at the UI level.
- Implement rate limiting on authentication endpoints.

## Dependencies

- Keep dependencies up to date. Automate vulnerability scanning.
- Pin dependency versions for reproducible builds.
- Audit new dependencies before adding them - check maintenance status, download counts, and known vulnerabilities.

## Error Handling

- Never expose stack traces or internal details to end users.
- Log errors with enough context to debug, but sanitize sensitive data from logs.
- Use generic error messages for users, detailed ones for logs.
