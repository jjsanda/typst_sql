# Security policy

quarry executes SQL over user-supplied database files inside the Typst
compiler and, via its sidecar, against remote databases. We treat memory
safety, sandbox integrity, read-only enforcement and credential hygiene as
security surfaces. The full model: docs/security-model.md.

## Reporting a vulnerability

Please report suspected vulnerabilities privately via GitHub Security
Advisories ("Report a vulnerability" on the repository) rather than a public
issue. Include a reproduction if you can — a database file, document or query
that demonstrates the problem.

We aim to acknowledge reports within 72 hours and to ship a fix or a
mitigation before public disclosure. Credit is given unless you prefer
otherwise.

## Scope

In scope: anything reachable from a hostile database file, hostile SQL,
hostile CBOR, or the sidecar's handling of credentials and caches.
Out of scope: bugs requiring a compromised toolchain, or Typst-level issues
(report those to Typst).
