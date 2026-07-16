# GitHub Setup

Repository: `MajidAsghariTabrizi/anti-gravity-phoenix-v4`

## Actions Settings

In GitHub web UI:

1. Open `Settings -> Actions -> General`.
2. Allow GitHub Actions for the repository.
3. Set workflow permissions to read repository contents by default.
4. Disable broad write permissions by default. The image build workflow grants `packages: write` only where needed.
5. Keep pull request workflows from untrusted forks away from production secrets.

## Repository Secrets

Create only these production deployment Actions secrets:

- `PROD_HOST`
- `PROD_PORT`
- `PROD_USER`
- `PROD_SSH_PRIVATE_KEY`
- `PROD_KNOWN_HOSTS`

Do not create `SIGNER_PRIVATE_KEY` in GitHub.

Create a protected GitHub environment named `production-shadow`. Require manual approval where the repository plan supports environment reviewers, and scope the five deployment secrets to that environment where possible.

Create a dedicated deploy SSH key on the production host. Grant it only the access needed to stage release artifacts and use non-interactive `sudo` for the reviewed asset installer, bootstrap, and deploy scripts. Do not grant a general interactive root shell. Verify the production host key out-of-band, then store the trusted host key text in `PROD_KNOWN_HOSTS`. Do not use deployment-time `ssh-keyscan` as identity verification.

Deployment remains manual even after the image workflow succeeds. The operator must supply current-main release evidence, an active rollback release, and the exact `DEPLOY_PRELIVE_SHADOW` acknowledgement. A release that changes protected feed-ingestor or Recorder digests is blocked before SSH.

The separately reviewed `Deploy PRE-LIVE Protected Maintenance` workflow uses
the same `production-shadow` environment and five deployment secrets. It also
requires exact release and rollback SHAs and build runs, plus
`DEPLOY_PRELIVE_PROTECTED_MAINTENANCE`. It is pinned to the immutable v3/v2
maintenance pair and is not a generic protected-service override. Keep
environment approval enabled for this workflow as well.

## Merge Settings

In `Settings -> General -> Pull Requests`:

1. Enable squash merging.
2. Disable merge commits if you want a single clean history unit per PR.
3. Require conversation resolution before merging.
4. Do not require a second reviewer while Majid is the sole developer and repository owner.

## Main Branch Ruleset

After the `Phoenix CI` workflow has run once and the checks exist in GitHub, create a branch ruleset for `main`:

- Restrict deletions.
- Block force pushes.
- Require a pull request before merge.
- Require conversation resolution.
- Require status checks:
  - `hygiene`
  - `go`
  - `rust-phoenix`
  - `rust-rpc-gateway`
  - `rust-recorder`
  - `rust-replay`
  - `rust-fork-sandbox`
  - `solidity`
  - `python-dashboard`
  - `docker-validation`
  - `integration-fixtures`
  - `jetstream-integration`

Require branches to be up to date only if the resulting queue/merge behavior does not deadlock the sole-owner workflow.
