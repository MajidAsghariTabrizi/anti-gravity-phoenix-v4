# Lesson L-006 — Sudo env reset

- **Incident**: stale SHADOW shell values survived atomic mode changes and selected the wrong healthcheck branch.
- **Root cause**: environment state inherited across privilege/mode transitions.
- **Evidence**: `docs/release-incident-history.md` item 3 (environment reloaded after atomic mode changes); item 4 (LIVE healthcheck explicitly uses the LIVE overlay).
- **Rule**: after privilege or mode changes the environment must be re-established explicitly; native remote tools never rely on inherited env across commands, and `sudo` is refused on the read-only path entirely.
- **Regression test**: `tests/native-tools.test.mjs` (remote tool refuses any command containing sudo/su/bash).
