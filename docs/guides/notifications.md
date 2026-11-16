# Alert notifications

vigily raises an alert when an evaluation suite drifts or a scheduled run fails. This guide sends
those alerts where people will see them: Slack, Microsoft Teams, Discord, any HTTPS endpoint, or
e-mail. Everything here can be done in the web app (**Settings → Notifications**), with the
`vigily` command, or over the [HTTP API](http-api.md#notification-channels); all three use the same
engine routes.

## Concepts

- A **channel** is one named destination of one kind: `slack`, `teams`, `discord`, `webhook` or
  `email`. There can be several of each (`ml-alerts`, `on-call mail`, `release bridge`).
- A channel's **secrets** (webhook URLs, the signing key, the SMTP password) are write-only. They
  are stored encrypted, or read from an environment variable of the engine when it sends; no
  answer, page or command ever shows a value, only its state: `stored …abcd`, `from env NAME` or
  `not set`.
- **Routing rules** decide which alerts go to which channels. After the rules, every alert also
  goes to the channels marked **default**, unless a matching rule stops it. With no rules, every
  alert goes to the default channels.
- Each alert is **delivered** once to each channel it is routed to, with retries. The delivery log
  is on the alert (Alerts page, "Delivery: 2 sent, 1 failed") and in
  `GET /api/v1/alerts/{id}/deliveries`.

A first channel takes one command. In a terminal on the machine that runs the engine (or with
`--url` and an admin token for a team server):

```bash
vigily channels add ml-alerts --kind slack --default
# webhook_url (not shown): paste the Slack webhook URL
vigily channels test ml-alerts
```

## Channel kinds

Every channel takes a name, `--default` (the default rule sends to it), `--disabled`, and a rate
limit: `--burst` messages at once (5 by default), then `--per-minute` (20 by default). Alerts over
the limit wait for their turn; none are dropped. Messages carry the title, the severity, what
changed, the experiment, suite, run and schedule, the time in UTC and a link into the web app (the
engine's `--public-url`, or `VIGILY_PUBLIC_URL`, else its own address).

### Slack

1. In Slack, create an app (or open an existing one) and turn on **Incoming Webhooks**.
2. **Add New Webhook to Workspace**, pick the channel that should receive alerts, and copy the URL
   (`https://hooks.slack.com/services/…`).
3. Add the channel and paste the URL when asked:

```bash
vigily channels add ml-alerts --kind slack --default
```

Messages use Block Kit: a header, the detail, the facts as fields and a button into the web app.

### Microsoft Teams

1. In the Teams channel, open **Workflows** and add the template **Post to a channel when a webhook
   request is received** (or an incoming webhook connector where it is still available).
2. Copy the URL the workflow gives.
3. `vigily channels add eval-team --kind teams` and paste it.

Messages are Adaptive Cards with the facts and an "Open in vigily" action.

### Discord

1. In Discord, open the channel's settings, then **Integrations → Webhooks → New Webhook**.
2. **Copy Webhook URL**.
3. `vigily channels add releases --kind discord` and paste it.

Messages are embeds, red for critical alerts and amber for warnings.

### Webhook

A `webhook` channel posts vigily's own JSON document to any HTTP endpoint: a pager, a chat bot, an
internal service. The document's schema is `vigily.alert/1`, and fields are only ever added to it
([the full document](http-api.md#the-webhook-document)):

```json
{
  "event": "vigily.alert",
  "schema": "vigily.alert/1",
  "test": false,
  "alert": {
    "id": "00001794498300556243-381f85",
    "kind": "behaviour_drift",
    "severity": "critical",
    "title": "accuracy dropped on intent-clf-v2",
    "detail": "accuracy is 0.810 against 0.900 on the same 200 items ...",
    "suite": "916d93593f1a",
    "run_id": "01a1d06ee5c1732fb3d654b9a0808e5b",
    "schedule": "nightly-intents",
    "created_at": 1794498300556243,
    "time": "2026-11-12T15:45:00Z",
    "dedupe": "behaviour/916d93593f1a/01a1d06ee5c1732fb3d654b9a0808e5b/accuracy"
  },
  "url": "https://vigily.example.com/drift/916d93593f1a"
}
```

Give it a **signing key** so the receiver can check that a request came from vigily. Generate one,
give the same value to the receiver, and store it with the channel:

```bash
openssl rand -hex 32                     # the signing key; the receiver keeps a copy
vigily channels add incident-bridge --kind webhook --secret signing_key
# url (not shown): https://bridge.example.com/hooks/vigily
# signing_key (not shown): the key
```

Each request then carries `X-Vigily-Timestamp` (Unix seconds) and
`X-Vigily-Signature: sha256=<hex>`, the HMAC-SHA256 of `<timestamp>.<raw body>` under the key. A
receiver recomputes it over the raw body (before parsing the JSON), compares in constant time, and
refuses old timestamps so a captured request cannot be replayed later:

```python
import hashlib
import hmac
import time


def from_vigily(key: bytes, headers, body: bytes, max_age: int = 300) -> bool:
    ts = headers.get("X-Vigily-Timestamp", "")
    sig = headers.get("X-Vigily-Signature", "")
    if not ts.isdigit() or abs(time.time() - int(ts)) > max_age:
        return False
    want = "sha256=" + hmac.new(key, ts.encode() + b"." + body, hashlib.sha256).hexdigest()
    return hmac.compare_digest(want, sig)
```

The same check in a shell, for a request saved to `body.json`:

```bash
printf '%s.' "$TIMESTAMP" | cat - body.json | openssl dgst -sha256 -hmac "$SIGNING_KEY"
```

Answer with any 2xx status. A 5xx, 408, 429 or no answer is retried; any other status fails the
delivery at once, and its status code is in the log.

### E-mail

vigily sends a plain-text and an HTML part per alert over SMTP:

```bash
vigily channels add "on-call mail" --kind email \
  --host smtp.example.com --security starttls \
  --username vigily --secret password \
  --from "vigily <vigily@example.com>" --to oncall@example.com --to ml-team@example.com
```

`--security` is `starttls` (port 587 by default), `tls` (465) or `none` (25, only for a relay on a
trusted network); `--port` overrides the port. The password is optional, for servers that need no
authentication. The delivery log shows the SMTP reply code of a refusal.

## Secrets

### Entering values

Values are never command-line arguments, where shell history and process listings would keep
them. `vigily channels add` asks for the required one (`webhook_url`, or `url` for a webhook)
without echo; `--secret FIELD` asks for others too. In scripts, `--stdin` reads the values from
standard input instead, one line per value in the order `webhook_url` or `url`, `signing_key`,
`password`:

```bash
printf '%s\n' "$SLACK_WEBHOOK_URL" | vigily channels add ml-alerts --kind slack --stdin
vigily channels edit ml-alerts --secret webhook_url          # replace the stored value
vigily channels edit incident-bridge --remove-secret signing_key
```

The web app's fields work the same way: a stored value can be kept, replaced, pointed at an
environment variable or removed, but never shown.

### Environment variables instead of stored values

A secret can also stay in the engine's environment: the channel stores only the variable's name,
and the engine reads it each time it sends.

```bash
vigily channels add ml-alerts --kind slack --env webhook_url=SLACK_WEBHOOK_URL
```

This works without an encryption key, and suits deployments that already inject secrets as
environment variables. A test send names the variable when it is not set.

### The encryption key

Stored values are encrypted with XChaCha20-Poly1305 under a key derived from `VIGILY_SECRET_KEY`,
each bound to its name ([details](configuration.md#secrets)).

- A **local engine** generates its key on first use (`engines/<key>.secret-key` in the state
  directory, readable only by its owner), so storing works without setup.
- A **server-mode engine** needs `VIGILY_SECRET_KEY` (or `VIGILY_SECRET_KEY_FILE`). Without it,
  storing a value is refused with a message saying how to set the key, and `env:NAME` references
  keep working.

```bash
vigily secrets status
```

```text
Encryption key: VIGILY_SECRET_KEY
Previous key: not set

CHANNEL       SECRET       STATE              CHANGED
ml-alerts     webhook_url  stored …Xk2q       admin, 2026-11-16 09:12 UTC
on-call mail  password     env SMTP_PASSWORD
```

**Docker secrets.** Every key variable also reads from a file named by `<NAME>_FILE`, so the key
need not be in the container's environment:

```yaml
services:
  vigily:
    environment:
      VIGILY_SECRET_KEY_FILE: /run/secrets/vigily_secret_key
    secrets: [vigily_secret_key]
secrets:
  vigily_secret_key:
    file: ./secret-key.txt        # openssl rand -hex 32 > secret-key.txt; chmod 600
```

**Rotating the key.** Restart the engine with the new key in `VIGILY_SECRET_KEY` and the old one in
`VIGILY_SECRET_KEY_PREVIOUS`. At start it encrypts every value again with the new key; after that
start, remove the old key. A value that neither key opens shows as undecryptable
(`vigily secrets status`, the web app and a warning at start): store it again, or start once more
with the key it was stored with as the previous key. Losing the key loses the stored values, not
the channels: store each value again.

## Routing rules

Rules are an ordered list. Each matches alerts by any of:

| Field | Matches |
|---|---|
| `kinds` | `behaviour_drift`, `data_drift`, `schedule_failed` |
| `severities` | `warning`, `critical` |
| `experiment` | a glob on the experiment (project) name |
| `suite` | a glob on the suite's name or key |
| `schedule` | a glob on the schedule's name |

A field left out matches every alert; globs take `*` (any characters) and `?` (one). Every matching
rule adds its channels. After the list, the alert goes to the channels marked default too, unless a
matching rule has `stop`. A rule with `stop` and no channels silences what it matches.

Edit them in the web app, or as a file:

```bash
vigily rules show > rules.toml     # the current rules, with their version
$EDITOR rules.toml
vigily rules set rules.toml
vigily rules list
```

```toml
version = 3

# Critical drift pages the bridge and the ML channel, and nothing else.
[[rules]]
name = "Critical drift"
channels = ["incident-bridge", "ml-alerts"]
stop = true
[rules.match]
kinds = ["behaviour_drift", "data_drift"]
severities = ["critical"]

# Failed nightly schedules go to e-mail as well as the default channels.
[[rules]]
name = "Nightly failures"
channels = ["on-call mail"]
[rules.match]
kinds = ["schedule_failed"]
schedule = "nightly-*"

# Nothing from sandbox experiments.
[[rules]]
name = "Quiet sandbox"
stop = true
[rules.match]
experiment = "sandbox-*"
```

Channels are named (ids work too). `vigily rules set` also takes JSON (a `.json` file, or `-` for
standard input) with the same fields; `vigily rules show --json` writes it. The version makes the
save safe when two people edit at once: if the rules changed since the file was read, nothing is
saved and the command says so; show them again and reapply the change. A file without `version`
replaces whatever is there.

## Delivery

- **Retries:** no answer, a 5xx, 408 or 429 is retried five times in all, from 30 seconds doubling
  to at most 5 minutes, with jitter. Pending deliveries survive a restart.
- **Rate limit:** per channel, `burst` then `per_minute`. A test send counts too.
- **Once per alert and channel:** an alert is raised once per drifted run and score or feature, and
  delivered once to each channel.
- **Disabled channels** get nothing; rules may still name them.

`vigily channels test NAME` sends one test alert, without retries, and prints the status code or
the reason verbatim; `vigily channels list` shows each channel's last delivery. Errors name only
the target's scheme, host and port, never a path or query that may hold a token.

## Internal addresses (server mode)

An engine in server mode resolves each Slack, Teams, Discord and webhook target before sending and
refuses loopback, private (10/8, 172.16/12, 192.168/16), shared (100.64/10), link-local
(169.254/16, fe80::/10) and unique-local (fc00::/7) addresses, so a channel added through the API
cannot reach services on the engine's own network. It connects only to the addresses it checked
and follows no redirects. To send to internal services, set:

```toml
[notifications]
allow_private_networks = true
```

A local engine allows any address, so a receiver on `localhost` works for testing.

## The deprecated `[alerts] webhook`

`[alerts] webhook = "https://..."` in vigily.toml still works: the engine serves it as the
read-only channel `config`, marked default, posting the same `vigily.alert/1` document. It cannot
be changed or deleted through the API or the web app, but it can be tested and named in rules.
Replace it with a webhook channel, then remove the line and restart:

```bash
vigily channels add hooks --kind webhook --default --secret signing_key
```

## Who can do what

Reading channels, rules, secret states and deliveries needs the `read` scope; adding, changing,
testing and deleting channels, saving rules and storing secrets need `admin`. Users signed in
without `admin` see the Notifications page read-only. Every change to a channel, a secret or the
rules is recorded with who made it, never a value (`GET /api/v1/audit`, admin).
