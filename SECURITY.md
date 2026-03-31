# Security

We take security seriously but keep the process small and practical. If you
find a vulnerability in AGOS Proxy, please report it responsibly.

## Reporting a vulnerability

Please do **not** open a public issue for security vulnerabilities.

Send an email to:

```
security@ivos.dev
```

Include:

- A description of the vulnerability and its impact.
- Any steps to reproduce it.
- Any relevant logs, request/response pairs, or proof of concept.

We will:

1. Acknowledge receipt within 5 business days.
2. Investigate and, if valid, prepare a fix.
3. Coordinate a responsible disclosure timeline with you.
4. Publish a security advisory once the fix is released.

## Scope

In scope:

- The AGOS Proxy binary and its CLI.
- The embedded store, crypto subsystem, and auth model.
- The HTTP API surface.
- The documentation, if it contains instructions that could lead to unsafe
  configurations.

Out of scope:

- Third-party providers (DeepSeek, Anthropic, Google, OpenAI, OpenRouter, etc.)
  — report issues to them directly.
- Vulnerabilities in dependencies — report to the dependency maintainers and,
  if the issue is dangerous and actively exploitable through AGOS Proxy, also
  tell us so we can mitigate or pin.
- Issues that require root access to the host or physical access to the machine.

## What we provide in return

- Credit in the release notes and security advisory, unless you prefer to stay
  anonymous.
- A timeline for the fix and disclosure, agreed with you.
- No retaliation — reporting in good faith is always welcome.

## Current known issues

See the issue tracker for the latest. Search for the `security` label.

## Security-relevant configuration

If you are deploying AGOS Proxy, the most important security decisions are:

- Bind address (`--bind`).
- TLS (use a front-end proxy, not AGOS Proxy itself).
- Profile passwords (for interactive management).
- Provider token handling (encrypted at rest, but protect the store file).
- Rate limits (per-profile, local only today).

See `docs/security.md` for the full threat model.
