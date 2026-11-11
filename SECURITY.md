# Security policy

## Supported versions

vigily is pre-1.0. Security fixes go into the latest release.

## Reporting a vulnerability

Please do not open a public issue. Report vulnerabilities privately through
[GitHub security advisories](https://github.com/sayef/vigily/security/advisories/new).

Include what you found, how to reproduce it and what you think the impact is. We will acknowledge the
report within five working days and keep you informed until it is resolved. We credit reporters in the
advisory unless you prefer otherwise.

## Scope

- `vigily up` serves runs, parameters and metrics without authentication. By default it binds to
  `127.0.0.1` and rejects requests whose `Host` header is not a loopback name, so a web page cannot
  read the API through DNS rebinding. With `--host` set to another address it serves anyone who can
  reach it; put it behind your own access control.
- The API can change a run's name, experiment, notes, tags and archived flag, add human labels to eval
  results, set drift baselines and acknowledge alerts. It cannot delete data.
- Scheduled commands run as the user that runs vigily, with its permissions and environment. They
  come only from the server's configuration file; the API cannot create, change or start them.
  Processes they leave behind are stopped when they exit or time out (see DESIGN.md for the limits).
- Command output (up to 64 KiB per stream) is kept for the newest runs and shown in the web app; it
  may contain secrets a command prints. With `--host` set to a non-loopback address, vigily warns at
  start that it serves without sign-in.
- Alert webhooks receive alert titles and details, which name targets, scores and schedules.
- The repository is trusted: vigily reads whatever kv0 reads at the configured location, with
  the credentials of the process.
- The Python SDK records the Python version, platform, CPU count, vigily version and git branch and
  commit of the training process. It does not record user names, host names, paths or environment
  variables.
