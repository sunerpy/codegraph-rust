# Security Policy

## Supported versions

Security fixes are applied to the current `main` branch and the newest published
release. Older releases do not receive guaranteed backports. Upgrade to the
newest verified release before reporting an issue already fixed on `main`.

## Report a vulnerability privately

Do **not** open a public issue for a vulnerability or include exploit details in
a public discussion. Use GitHub Private Vulnerability Reporting:

<https://github.com/sunerpy/codegraph-rust/security/advisories/new>

Include, when available:

- affected version, commit, platform, and installation method;
- minimal reproduction and expected/observed behavior;
- impact and required attacker capabilities;
- whether the issue crosses a project-root, filesystem, process, MCP/HTTP, agent
  configuration, archive/checksum, or release boundary;
- logs with secrets and personal paths removed;
- a proposed fix or regression test, if you have one.

You should receive an acknowledgement within seven days. Disclosure timing will
be coordinated after the report is reproduced and a fix/release path exists.
Please do not publish details before that coordination completes.

## Security boundaries

CodeGraph performs static analysis and reads source in the selected project. Its
index is not a sandbox and should be protected like derived source code. MCP
clients and local HTTP users receive source excerpts and graph data according to
the project path they request.

The project aims to keep managed paths within the selected project, reject
ambiguous resolution, keep MCP stdout protocol-clean, verify release checksums,
and fail closed on unsafe stale source or inconsistent state. These properties do
not make untrusted repositories safe to execute: do not run project scripts,
build hooks, or generated binaries merely to index source.

For ordinary bugs and hardening ideas with no confidential exploit details, use
the public issue templates.
