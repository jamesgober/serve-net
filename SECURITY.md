# Security policy

## Reporting a vulnerability

Email **me@jamesgober.com** with "serve-net security" in the subject. Please include the version, a
description of the issue, and a way to reproduce it. Do not open a public issue for a vulnerability.

You will get an answer within three working days. A confirmed issue gets a fixed release, a GitHub
security advisory, and a credit in the release notes if you want one.

## Supported versions

serve-net is pre-1.0. Fixes go into the newest release only; versions with a known vulnerability are
yanked from crates.io once a fixed release exists.

| Version | Supported |
|---|---|
| 0.6.2 and later | yes |
| 0.3.0 to 0.6.1 | no, yanked: [GHSA-7vvv-g6cw-p8gj](https://github.com/jamesgober/serve-net/security/advisories/GHSA-7vvv-g6cw-p8gj) |

## How serve-net is built to be secure

[`docs/SECURITY.md`](./docs/SECURITY.md) describes the trust boundaries, the defaults, what a
deployment should check, and what serve-net does not defend.
