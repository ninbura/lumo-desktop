# Bridge E2E suite

This suite drives the real desktop app through WebdriverIO. Calls travel through
clients' `desktop-tools.ts`, the injected desktop bridge, and the native Rust core.
It checks bridge behavior and observable side effects using a harness page, rather than
exercising the hosted chat UI.

## Embedded driver (native macOS, Windows, Linux)

Install the two sets of Node dependencies once, then run from the repo root:

```sh
npm ci
npm ci --prefix e2e
npm run test:e2e
```

Requires Node 22.19+, Rust, and the platform's normal Tauri build dependencies.
The live clients checkout defaults to `../clients`; set `CLIENTS_DIR` to override it.
On Linux, provide a display and a session bus (for example,
`xvfb-run -a dbus-run-session -- npm run test:e2e`). The embedded route needs no
`tauri-driver`, Edge WebDriver, or WebKitWebDriver installation.

Run selected specs by filename, or reuse the previously built test binary:

```sh
npm run test:e2e -- discovery.e2e.js auto-execute.e2e.js
npm run test:e2e -- --skip-build
```

The runner bundles the real clients harness plus WDIO initialization, then builds
with the optional `e2e-webdriver` Cargo feature into `src-tauri/target/e2e/`
(or `CARGO_TARGET_DIR`, when set).
Every spec gets a separate WDIO invocation, native process, temporary config and
scratch directory, and ephemeral webview storage. This prevents an embedded server's
long-lived application state from leaking across specs. Successful runs remove their
temporary state; failures retain state and logs at the printed location.

The feature excludes single-instance forwarding and autostart registration. It
requires `LUMO_E2E_TESTING` and an absolute `LUMO_E2E_CONFIG_DIR`; the runner supplies
both. Automation capabilities apply only to the local `lumo-window` stub. Normal
builds do not include either WDIO Rust plugin. Do not ship a test-feature binary.

`LUMO_E2E_TIMEOUT` bounds each spec invocation in seconds (default 600).
`LUMO_APPROVAL_TIMEOUT_MS` defaults to 5000. `LUMO_APP_BINARY` can override the test
executable path. `TAURI_DRIVER_PORT` selects the embedded server port (default 4445).
Only run one suite at a time with a given port.
`LUMO_E2E_ARTIFACT_DIR` selects the parent directory for per-spec state and logs
(default: the OS temporary directory).

Secure-storage detection expects `available` on macOS/Windows and `unavailable`
on Linux without Secret Service. Set `LUMO_E2E_EXPECT_SECURE_STORAGE` explicitly
for other runner environments. macOS needs an accessible keychain; the app's real
startup probe is exercised, rather than mocked.

## Linux container

```sh
./e2e/run.sh
CLIENTS_DIR=/path/to/clients ./e2e/run.sh
NO_CACHE=1 ./e2e/run.sh
LUMO_E2E_TIMEOUT=900 ./e2e/run.sh
./e2e/run.sh discovery.e2e.js auto-execute.e2e.js
./e2e/run.sh --skip-build
```

The container uses the same embedded runner and stub as native macOS and Windows.
WebKitGTK, Xvfb, D-Bus and xauth provide the Linux runtime; neither `tauri-driver`
nor `WebKitWebDriver` is installed. Docker's init process handles display-server
startup signals and reaps child processes.

Named volumes isolate Linux Node dependencies and Cargo artifacts from the host.
`CARGO_TARGET_DIR=/target` keeps builds cached between containers. Each spec uses
fresh settings and scratch directories; failure artifacts survive container removal
under `build/e2e/`. CI runs the same container entry point and uploads that directory
on failure. Rebuild the CI toolchain image before running the migrated job.

## Suite structure

| Location | Responsibility |
| --- | --- |
| `specs/` | Scenarios grouped by behavior |
| `support/waits.js` | Harness readiness, tool invocation, approval and result waits |
| `support/schedules.js` | Schedule lifecycle and execution through the real clients adapter |
| `support/security.js` | Security fixture paths, policy changes, approval reasons and execution-journal reads |
| `fixtures/mcp-server.cjs` | Real stdio MCP peer with read-only, outbound, write and unannotated tools |
| `support/fixtures.js` | App settings, account state, MCP configuration and legacy cache fixtures |
| `e2e_testing_stub.html` | DOM interface driven by the specs |
| `build-stub.mjs` | Bundles clients' `packages/lumo-api-client/e2e/desktop-bridge-harness.ts` into `src-tauri/dist/` |
| `wdio.conf.js` | Driver lifecycle, app environment and test configuration |
| `run.sh` / `run-in-container.sh` / `Dockerfile` | Local container launcher, shared execution steps and toolchain image |

The harness imports the real clients module on every build. Keep that source in the
clients repo; do not vendor a copy here. Generated harness and desktop scripts under
`src-tauri/dist/` are gitignored.

The app runs with `LUMO_E2E_TESTING=1`, which loads the harness page and registers the
native `EchoConnector` (`echo__auto` and `echo__ask`). App environment is inherited
from the embedded WebdriverIO service.

Fixtures use a fixed test account, disposable scratch space (`LUMO_E2E_SCRATCH`), and
configuration at `LUMO_E2E_CONFIG_DIR`. The runner seeds fresh settings, scratch,
connector state, and a schedule ledger for each spec. The allowed filesystem roots include scratch and the
configuration parent, allowing tests to verify that Lumo's own directory stays protected.
The MCP peer records executions in a scratch journal; its offline companion uses a
seeded legacy cache that startup discovery cannot overwrite. Both start disabled, so
their untrusted tool descriptions do not affect unrelated filesystem scenarios. Security
specs enable the peers they use and disable them on cleanup.

Policy cleanup writes explicit defaults (`ask`); there is no bridge API to remove an
override. MCP tools start without overrides when fixtures are seeded. The defaults
spec checks them before its user-override case, whose cleanup leaves an explicit `ask`.
These stdio peers count as hand-added servers: all tools default to asking, regardless
of their safety hints, and their results count as untrusted service content.

## Coverage

Maintain this table when coverage changes, rather than adding feature-specific sections.

| Behavior | Specs | Assertions |
| --- | --- | --- |
| Account identity and discovery | `account-identity.e2e.js`, `discovery.e2e.js` | Account resolution, refusal to guess ambiguous identities, and native tool discovery |
| Tool approvals | `auto-execute.e2e.js`, `approve.e2e.js`, `deny.e2e.js`, `timeout.e2e.js` | Automatic execution, approval, denial, timeout and pending-card cleanup |
| Filesystem access | `filesystem-write.e2e.js`, `filesystem-protected.e2e.js` | Approved writes, root enforcement, protected reads/writes/edits/searches, and neighboring allowed files |
| MCP policies | `mcp-policy.e2e.js` | Hand-added servers default to asking regardless of hints; user auto-approval yields to service exposure; legacy-cache reclassification and execution only after approval |
| Exposure-based approvals | `tool-exposure.e2e.js` | Shell and service results plus hand-added tool descriptions gate MCP calls and file changes despite auto-approve; local reads remain available |
| Schedule lifecycle | `schedules.e2e.js` | Serialization, durable pending reads, delivery and missed-event recovery, status reconciliation, connection expiry, and event subscriptions |
| Schedule permissions | `schedule-approval.e2e.js`, `schedule-exposure.e2e.js` | Run-scoped approval, disabled tools, connector allowlists, context expiry, exposure refusals including hand-added read-only claims, and isolation from ordinary chat and separate runs; tool descriptions respect the run's connector allowlist |
| Secure-storage availability | `secure-storage.e2e.js` | Storage availability matches the platform expectation |

Exposure scenarios use real shell output and the account-session fallback. Conversation
isolation, restart persistence, mail-triggered exposure, credential binding, and MCP
environment filtering are not covered here. Native link dialogs, PDF export, and microphone
permissions also remain outside this suite's coverage.

## Add or change tests

- Put scenarios in the spec for the behavior they exercise; add a spec when introducing
  a separate behavior. Keep reusable driver operations in `support/` and peers/data in
  `fixtures/`.
- Exercise the real client adapter, bridge and native enforcement. Wait for harness
  readiness and explicit results or events instead of fixed sleeps.
- Assert side effects independently: use Node's `fs` for file contents and the MCP
  execution journal for whether a call ran. An error response alone does not prove that
  execution was prevented.
- Use unique file names and call markers. Keep scenario setup independent of preceding
  tests and restore policy overrides after changing them.
- Update the coverage table and any affected fixture or harness documentation here.
  Run `./e2e/run.sh` before considering a change validated.

## Harness DOM reference

The stub is an automation interface. `support/waits.js` checks `#ready` and surfaces
`#error` while waiting, so failures report the underlying error rather than just a timeout.
The harness clears results before each action.

| Nodes | Contract |
| --- | --- |
| `#ready` | `1` after the clients harness is wired up |
| `#tools` | Comma-separated discovered tool names |
| `#pending`, `#pending-id` | Pending approval count and first request ID |
| `#result`, `#error` | Last tool results as JSON and uncaught harness errors |
| `#tool-name`, `#tool-args`, `#run` | Invoke a named tool with JSON arguments |
| `#approve`, `#deny` | Resolve the first pending approval |
| `#schedule-request`, `#schedule-run` | Submit a JSON schedule action through the clients harness |
| `#schedule-result` | `{ ok: true, data }` or `{ ok: false, error }` |
| `#schedule-notices` | Work-available events received by the schedule subscriber |
