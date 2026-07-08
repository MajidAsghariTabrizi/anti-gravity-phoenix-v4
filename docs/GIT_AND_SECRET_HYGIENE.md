# Git and Secret Hygiene

Phoenix should be safe to clone, inspect, and build without exposing operator credentials.

## Allowed In Git

- Source code, tests, migrations, fixtures, and documentation.
- `.env.example` with placeholders only.
- `Cargo.lock` files for application crates.
- `go.mod` and `go.sum` when present.
- Protocol registry examples with placeholders for unverified/operator-specific values.
- Dockerfiles, Compose files, Prometheus config, and CI scripts.

## Never Commit

- `.env`, `.env.local`, or `.env.*.local`.
- Ethereum private keys, mnemonics, wallet exports, keystores, PEM/PFX/P12 files, or signer material.
- API keys, RPC URLs containing embedded credentials, bearer/basic authorization values, bot tokens, and webhook secrets.
- Local databases, feed recordings, replay outputs, benchmark outputs, build outputs, caches, and runtime data directories.

## Run The Scans

PowerShell:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\secret-scan.ps1
powershell -ExecutionPolicy Bypass -File .\scripts\forbidden-file-check.ps1
```

Bash:

```bash
./scripts/secret-scan.sh
./scripts/forbidden-file-check.sh
```

Both secret scanners report only file, line number when available, category, and remediation. They must not print the secret value.

## If A Secret Is Committed

1. Revoke or rotate the compromised secret immediately.
2. Stop using the leaked value everywhere.
3. Remove the secret from the current source.
4. Rewrite Git history using an approved tool such as `git filter-repo` or BFG Repo-Cleaner.
5. Force-push only after coordinating with every collaborator and protected branch policy.
6. Ask every clone owner to reclone or garbage-collect old history.

Deleting a secret from the latest source file is not sufficient once it exists in Git history. Anyone with access to the repository history can still recover the old blob until history is rewritten and stale clones are cleaned up.
