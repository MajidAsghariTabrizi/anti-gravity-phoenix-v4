# Should deployment and execution activation be the same action?

> **Status:** Phoenix is `FULL_LIVE_NO_ALPHA`. This is a discussion of release engineering, not of operational claims.
>
> **Related Phoenix engineering note:**
> - [`protected-release-lifecycle.md`](../protected-release-lifecycle.md)

## Background

For most software, "merge → deploy → live" is correct. For money-touching software, it is wrong. The reasons are:

1. A bad deploy that goes live can lose money.
2. A deploy that goes live before the environment is ready can lose money.
3. The people who deploy are usually not the people who authorize execution. The two authorities should not be conflated.

Phoenix separates deployment from activation. The deploy script (`deploy-release.sh`) starts services in SHADOW mode by default. The activation gate is a separate workflow that cannot enable LIVE or receive signer material. The two operations are performed by different people, in different sessions, with different evidence requirements.

## Questions for the discussion

1. In your financial-infrastructure release pipeline, who has the authority to enable live execution? Is that the same authority as the deploy authority?
2. Can your deploy process produce a state where live execution is enabled? If yes, the deploy process has authority it should not have.
3. Are your production artifacts referenced by digest or by tag? Mutable tags can be moved. Digests cannot.
4. Can your CI produce a release manifest without a successful build? If yes, the manifest does not enforce build success.
5. What's the recovery time from a bad release? For Phoenix it is bounded by `rollback-release.sh` and a previous-release pointer. Is that the right shape?

## What this is not

This is not a claim that Phoenix has had zero bad releases. Phoenix has run SHADOW releases extensively, and the recent production audit captured 16 Docker containers, 4.35 GB of PostgreSQL state, 0 attempted live executions, and 0 realized PnL. The release model is in place; the live execution it would protect is not.

What this is, is a discussion of release engineering as a safety discipline. The position Phoenix takes is:

- Deployment is not activation.
- A bad deploy cannot enable live execution.
- A bad release can be rolled back through a release-shaped operation.
- Activation authority is separate from deployment authority.

What positions have others taken?