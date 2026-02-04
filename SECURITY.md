# Security Policy

## Supported Versions

| Version | Supported          |
| ------- | ------------------ |
| 0.1.x   | :white_check_mark: |

## Reporting a Vulnerability

We take security vulnerabilities seriously. If you discover a security issue, please report it responsibly.

**Please do NOT report security vulnerabilities through public GitHub issues.**

Instead, please send an email to **security@latentinfinity.com** with:

- A description of the vulnerability
- Steps to reproduce the issue
- Potential impact assessment
- Any suggested fixes (optional)

### What to Expect

- **Acknowledgment**: We will acknowledge receipt of your report within 48 hours.
- **Assessment**: We will investigate and assess the vulnerability within 7 days.
- **Resolution**: We aim to release a fix within 30 days for confirmed vulnerabilities.
- **Disclosure**: We will coordinate with you on public disclosure timing.

### Safe Harbor

We consider security research conducted in accordance with this policy to be authorized and will not pursue legal action against researchers who:

- Act in good faith
- Avoid privacy violations and data destruction
- Do not exploit vulnerabilities beyond what is necessary to demonstrate the issue
- Report vulnerabilities promptly

## Security Best Practices

When using md-crdt in your applications:

- Keep dependencies up to date
- Validate and sanitize any user-provided markdown before processing
- Use appropriate access controls for synchronized files
- Review the CRDT merge behavior for your specific use case

## Scope

This security policy applies to the md-crdt codebase and official releases. Third-party integrations and forks are not covered.
