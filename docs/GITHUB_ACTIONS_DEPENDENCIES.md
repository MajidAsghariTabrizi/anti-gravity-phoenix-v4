# GitHub Actions Dependencies

Pins were resolved from the official GitHub API tag refs on 2026-07-08. Update pins by fetching the official tag ref again, reviewing upstream release notes, opening a dependency PR, and letting CI prove the update. Dependabot may propose minor and patch updates, but there is no auto-merge.

| Action | Repository | Pinned SHA | Upstream tag | Reason |
| --- | --- | --- | --- | --- |
| checkout | `actions/checkout` | `34e114876b0b11c390a56381ad16ebd13914f8d5` | `v4` | Fetch repository source for CI, image builds, and LIVE readiness report. |
| setup-go | `actions/setup-go` | `40f1582b2485089dde7abd97c1529aa768e1baff` | `v5` | Install and cache the Go toolchain for feed-ingestor and migration-runner checks. |
| setup-python | `actions/setup-python` | `a26af69be951a213d495a4c3e4e4022e16d87065` | `v5` | Install Python for dashboard smoke checks. |
| upload-artifact | `actions/upload-artifact` | `ea165f8d65b6e75b540449e92b4886f43607fa02` | `v4` | Persist image manifest fragments and release/LIVE reports. |
| download-artifact | `actions/download-artifact` | `d3f86a106a0bac45b974a628896c90dbdf5c8093` | `v4` | Assemble release manifests and fetch the exact manifest for deployment. |
| setup-buildx | `docker/setup-buildx-action` | `8d2750c68a42422c14e847fe6c8ac0403b4cbd6f` | `v3` | Provide Docker Buildx for validation and GHCR image builds. |
| login-action | `docker/login-action` | `c94ce9fb468520275223c153574b00df6fe4bcc9` | `v3` | Authenticate to GHCR with `GITHUB_TOKEN` in the image publishing workflow. |
| build-push-action | `docker/build-push-action` | `10e90e3645eae34f1e60eeb005ba3a3d33f178e8` | `v6` | Build and push immutable Phoenix images with BuildKit cache and OCI labels. |
| foundry-toolchain | `foundry-rs/foundry-toolchain` | `b00af27efadbc7b4ca8b82abbd903b17cc874d2a` | `v1` | Install Foundry for Solidity formatting and tests. |

Source URLs used:

- `https://api.github.com/repos/actions/checkout/git/ref/tags/v4`
- `https://api.github.com/repos/actions/setup-go/git/ref/tags/v5`
- `https://api.github.com/repos/actions/setup-python/git/ref/tags/v5`
- `https://api.github.com/repos/actions/upload-artifact/git/ref/tags/v4`
- `https://api.github.com/repos/actions/download-artifact/git/ref/tags/v4`
- `https://api.github.com/repos/docker/setup-buildx-action/git/ref/tags/v3`
- `https://api.github.com/repos/docker/login-action/git/ref/tags/v3`
- `https://api.github.com/repos/docker/build-push-action/git/ref/tags/v6`
- `https://api.github.com/repos/foundry-rs/foundry-toolchain/git/ref/tags/v1`

Do not replace these with floating tags in workflows. If a pin cannot be resolved because network access is unavailable, use the maintained tag only as a temporary blocker and document the blocker in the PR.
