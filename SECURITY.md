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

- Every API request needs a token, except health, info and `auth/me`. In local mode (the default
  on `127.0.0.1`) that is a random per-repository token kept in a file readable only by its owner;
  vigily also rejects requests whose `Host` header is not a loopback name, so a web page cannot
  read the API through DNS rebinding. In server mode (any other address) it needs
  `VIGILY_ADMIN_TOKEN` from the environment, or `--insecure-no-auth`, and accepts tokens with the
  scopes `read`, `ingest` and `admin`, stored as SHA-256 hashes. The browser keeps its token in an
  HttpOnly, SameSite=Strict cookie, and a change made with the cookie must come from vigily's own
  origin. Put TLS in front of a server-mode engine.
- The API can change a run's name, experiment, notes, tags and archived flag, add human labels to eval
  results, set drift baselines and acknowledge alerts. It cannot delete data.
- Scheduled commands run as the user that runs vigily, with its permissions and environment. They
  come only from the server's configuration file; the API cannot create, change or start them.
  Processes they leave behind are stopped when they exit or time out (see DESIGN.md for the limits).
- Command output (up to 64 KiB per stream) is kept for the newest runs and shown in the web app; it
  may contain secrets a command prints. With `--insecure-no-auth`, vigily warns at start that it
  serves without sign-in.
- Alert webhooks receive alert titles and details, which name targets, scores and schedules.
- The repository is trusted: vigily reads whatever kv0 reads at the configured location, with
  the credentials of the process.
- The Python SDK records the Python version, platform, CPU count, vigily version and git branch and
  commit of the training process. It does not record user names, host names, paths or environment
  variables.
