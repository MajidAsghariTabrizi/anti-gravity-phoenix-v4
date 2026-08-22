# Lesson L-004 — Docker entrypoint mismatch

- **Incident**: extracted shell/Python assets failed on `noexec` mounts and container entrypoints diverged from the release tree.
- **Root cause**: implicit interpreters and unreviewed entrypoints silently select different code than the immutable release tree.
- **Evidence**: `docs/release-incident-history.md` items 10, 18 (explicit interpreters on noexec; context tooling version-matched to its immutable release tree).
- **Rule**: entrypoints and extracted assets use explicit interpreters; post-install hard gate verifies the running image identity against the release tree before activation.
- **Regression test**: `tests/release-graph.test.mjs` (release graph requires the hard-gate node between controller and activation).
