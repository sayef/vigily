# vigily-engine

The engine of [vigily](https://github.com/sayef/vigily), an experiment tracker: it owns a
repository's storage and serves its runs, evaluations and traces to the web app, the `vigily`
command line and the Python SDK over HTTP.

This package holds the `vigily-engine` binary alone. Install it with the SDK:

```bash
pip install 'vigily[local]'
```

The SDK then starts it in the background when no engine serves the repository. See the
[documentation](https://github.com/sayef/vigily) for everything else.
