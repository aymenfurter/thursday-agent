<p align="center">
  <img src=".github/extensions/thursday/logo.svg" width="160" height="160" alt="thursday-agent door logo">
</p>

<h1 align="center">thursday-agent</h1>

<p align="center">
  <strong>One voice. Two workers. Your Mac.</strong><br>
  A macOS voice assistant with GitHub Copilot workers and native window effects.
</p>

<p align="center">
  <img src="https://img.shields.io/badge/macOS-14%2B-1f2328?logo=apple&amp;logoColor=white" alt="macOS 14 or later">
  <img src="https://img.shields.io/badge/Rust-core-a45b2b?logo=rust&amp;logoColor=white" alt="Rust core">
  <img src="https://img.shields.io/badge/Swift%20%2B%20Metal-native%20effects-b83b24?logo=swift&amp;logoColor=white" alt="Swift and Metal effects">
  <img src="https://img.shields.io/badge/GPT--Live-Azure%20%2F%20OpenAI-0067b8" alt="GPT-Live through Azure or OpenAI">
  <img src="https://img.shields.io/badge/GitHub%20Copilot-SDK-6e40c9?logo=githubcopilot&amp;logoColor=white" alt="GitHub Copilot SDK">
</p>

<p align="center">
  <a href="#quick-start">Quick start</a> &middot;
  <a href="#architecture">Architecture</a> &middot;
  <a href="#copilot-app-canvas">Canvas</a> &middot;
  <a href="#voice-providers">Providers</a> &middot;
  <a href="#command-line-reference">CLI flags</a>
</p>

Speak a request to open an app, work with files, use a browser, or ask about your
Copilot App sessions. GPT-Live handles the conversation. Two persistent Copilot
workers handle the work:

| Role | Default model | Responsibility |
| --- | --- | --- |
| Voice | `gpt-live-1` | Listen, speak, and delegate computer actions. |
| Fast worker | `gpt-5.6-luna` | Handle short requests and coordinate longer tasks. |
| Deep worker | `gpt-6-astra` | Complete longer tasks and accept follow-up instructions. |

## Architecture

<p align="center">
  <a href="thursday-architecture.png">
    <img src="thursday-architecture.png" width="960" alt="thursday-agent architecture: Azure or OpenAI GPT-Live connects to a Rust core, two GitHub Copilot workers, Peekaboo computer controls, a Swift overlay helper, and the Copilot App canvas through an authenticated local bridge.">
  </a>
</p>
<p align="center"><sub>Select the diagram to view it at full resolution.</sub></p>

## Quick start

**Requirements:** macOS 14 or later, Rust and Cargo, Xcode command-line tools
with Swift, Node.js with `npx`, GitHub Copilot access, and access to a GPT-Live
model through Azure OpenAI or OpenAI. Azure also requires Azure CLI.

From the project folder, build the Rust binary and native helper, then sign in:

```sh
scripts/build.sh
copilot login
```

For Azure, also run `az login`. Choose a startup path:

| From the Copilot App | From the terminal |
| --- | --- |
| Open a session for this folder, reload extensions, and open the **thursday-agent** canvas. Follow the [canvas setup](#copilot-app-canvas). | Run the commands below to save preferences, check the voice connection, and start. |

```sh
./target/release/thursday-agent setup
./target/release/thursday-agent voice-check
./target/release/thursday-agent run
```

> [!IMPORTANT]
> thursday-agent uses **GPT-Live**, not GPT-Realtime. Chat, embedding, and
> GPT-Realtime models do not implement its voice protocol.

The canvas uses `target/release/thursday-agent` from this folder. It does not install
packages or build the binary when you select a button. Stop and start thursday-agent
after a rebuild. The canvas refuses to start a binary older than the Rust source
or Cargo files and shows the rebuild command instead. The build script sets
`--target-dir target` so `CARGO_TARGET_DIR` cannot redirect the binary to another
folder.

## Copilot App canvas

The project extension is in [`.github/extensions/thursday/`](.github/extensions/thursday/).
Reload extensions in a Copilot App session for this folder, then open **thursday-agent**.

The canvas and this README use the same door-only [SVG logo](.github/extensions/thursday/logo.svg).
The product name is separate text, not part of the logo.

1. Open **Setup** and choose a provider. For Azure, select a tenant and
   subscription, load resources, then choose a resource and deployed GPT-Live model.
2. Select **Save model**, then **Diagnosis > Check connection**.
3. Select **Voice > Start thursday-agent**. This enables the microphone and Mac
   controls. Select **Stop thursday-agent**, or close the last thursday-agent canvas, to stop.

Stop also cancels a start that is still checking permissions. A model save,
connection check, or voice start cannot run while another of these operations
is pending, or while voice is running. Conflicting requests return an error;
they are not queued. Stop does not cancel a connection check.

| Tab | What it contains |
| --- | --- |
| **Voice** | Saved model and start/stop controls. Opens by default when a model is saved. |
| **Setup** | Provider, tenant, subscription, resource, and model dropdowns. Opens when no model is saved. Subscription search appears for tenants with more than eight subscriptions. |
| **Diagnosis** | Connection check, Mac permissions, and process log. **Process details** shows paths, PIDs, and check times. |

The process log is plain text. Terminal colours are used only when thursday-agent runs
in a terminal; the canvas also removes terminal control codes from captured logs.

Azure is preferred unless an explicit OpenAI selection was saved. The tabs
support arrow keys, Home, and End.

OpenAI is disabled until `OPENAI_API_KEY` is available to the app's extension
process. The runtime can ask for permission to pass this variable to the
extension. If the running app does not have it, restart the app with the variable
supplied.

Other Azure deployments remain visible but disabled. Discovery reads one
subscription at a time; it does not scan every cached subscription or change the
CLI's default subscription. Discovery errors are shown, not treated as empty results.

## Voice providers

| Provider | Authentication | Selection |
| --- | --- | --- |
| **Azure OpenAI** | Azure CLI sign-in and short-lived bearer tokens. No Azure API key is needed. | Preferred when `AZURE_OPENAI_ENDPOINT` is set, unless a saved selection or CLI flag overrides it. |
| **OpenAI** | `OPENAI_API_KEY` supplied through your secret manager, or a private CLI setup file. | Select OpenAI in the canvas or pass `--provider openai`. |

An Azure authentication error **does not** switch the connection to OpenAI.

<details>
<summary><strong>Azure CLI configuration example</strong></summary>

Replace the placeholders with your own resource, subscription, tenant, and
deployment. No Azure resource or account IDs are built into thursday-agent.

```sh
az login
export AZURE_OPENAI_ENDPOINT="https://<resource-name>.services.ai.azure.com"
export AZURE_OPENAI_DEPLOYMENT="gpt-live-1"
export AZURE_OPENAI_SUBSCRIPTION="<subscription-id>"
export AZURE_OPENAI_TENANT="<tenant-id>"
./target/release/thursday-agent voice-check
```

The signed-in identity needs access to the deployed model. thursday-agent obtains a
token for `https://cognitiveservices.azure.com/` each time it connects. It does
not write access tokens to disk or display them.

Environment values do not override a saved model selection. Change it in the
canvas or use explicit CLI flags. Azure requires an endpoint from your selection,
configuration, environment, or CLI flags. If subscription and tenant are omitted,
Azure CLI uses its current account.

</details>

> [!CAUTION]
> Do not put literal API keys in shell startup files or commit a private
> configuration file. `thursday-agent setup` can store an OpenAI key locally; Azure
> authentication uses Azure CLI instead.

### Saved configuration

The saved `live_selection` contains the provider, deployment/model, endpoint,
subscription, and tenant, but no token. Voice selection follows this priority:
**explicit CLI options > saved selection > environment defaults**.

Preferences use `THURSDAY_CONFIG` when set. Otherwise, macOS uses:

```text
~/Library/Application Support/thursday/config.json
```

The existing configuration directory, `THURSDAY_*` environment variables,
extension directory, canvas ID, bridge tool names, and helper bundle ID stay
unchanged so saved settings, open canvases, and integrations keep working.
The executable and all product labels use `thursday-agent`.

## Command-line reference

Use `./target/release/thursday-agent` in place of `thursday-agent` if the binary is not on your PATH.

| Command | Purpose |
| --- | --- |
| `thursday-agent run` | Start listening. This is also the default command. |
| `thursday-agent setup` | Select a voice provider and save preferences. |
| `thursday-agent voice-check` | Open a real GPT-Live session, wait for `session.started`, then close it. No microphone, workers, or computer tools. |
| `thursday-agent doctor` | Check voice credentials, Copilot login, permissions, the helper, and audio. Opens the microphone and speakers; the helper can show an overlay. |

Flags are global and can appear before or after a command.

| Flag | Purpose |
| --- | --- |
| `--provider <azure\|openai>` | Select the voice provider. |
| `--endpoint <URL>` | Set the resource root or full secure live WebSocket URL. |
| `--live-model <NAME>` | Set the voice model or Azure deployment. Default: `gpt-live-1`. |
| `--azure-subscription <ID>` | Select the subscription for Azure CLI token acquisition. |
| `--azure-tenant <ID>` | Select the tenant, or verify it when a subscription is supplied. |
| `--workspace <PATH>` | Set the folder where the workers operate. |
| `--quick-model <MODEL>` | Set the fast worker model. Default: `gpt-5.6-luna`. |
| `--deep-model <MODEL>` | Set the deep worker model. Default: `gpt-6-astra`. |
| `--fast-reasoning <LEVEL>` | Set fast worker reasoning. Default: `none`. |
| `--reasoning <LEVEL>` | Set deep worker reasoning. Default: `low`. |
| `--no-helper` | Disable native glow and window highlights. |
| `--quiet` | Hide the live debug console. |
| `--verbose` | Also write logs to stderr. |
| `--help` / `--version` | Show help or the installed version. |

For example:

```sh
./target/release/thursday-agent run --workspace "$PWD" --provider azure --quiet
```

## Mac permissions

In **Diagnosis > Mac permissions**, check **Accessibility**, **Screen Recording**,
and **Event Synthesizing**. Each state is shown as granted, not granted, or unknown.

Grant missing permissions in **System Settings > Privacy & Security** for the
app that launches thursday-agent. Event Synthesizing uses the Accessibility setting.
Different app builds can have different grants. Quit and reopen that app after
changing its permissions, then check again.

Missing Mac control permissions do not block voice-only or app-session requests.

<details>
<summary><strong>How the process-aware permission check works</strong></summary>

The canvas identifies its current extension PID, walks its process ancestry to
find the host app, then runs the configured local Peekaboo command with
`permissions --json --no-remote`. It checks on canvas open, before each voice
start, and when you select **Check permissions**.

The default `npx` check uses cached packages only; it does not install or update
Peekaboo. **Process details** includes the host app name, executable paths, PIDs,
and check time. Errors and remote-host reports are not treated as grants.

The local controller child process reports permissions from the extension's
launch context. This is not a macOS API for querying arbitrary PIDs. The host app
label comes from process ancestry.

</details>

## Ask about app sessions

thursday-agent's workers use `ask_copilot_app` to ask the connected app agent for
information or a user-requested action. Try:

> "List all sessions and their current activity."
>
> "Get the last activity of session X."
>
> "Ask session X to report its current blocker."

The connection is agent-only; there is no prompt form in the canvas. Changes or
messages to other sessions require a user request. The bridge does not bypass
the app's permissions or grant direct database access.

<details>
<summary><strong>Request and reply protocol</strong></summary>

Each prompt goes to the app session that owns the canvas. Fixed instructions
require the agent to answer as quickly as possible through
`thursday_reply({ requestId, response })`, with either an answer or a blocker.
The agent then stops. It must not narrate, post the answer in chat, render widgets,
open canvases, or create presentation files. The host may still show the request
and tool activity in its timeline.

The explicit reply completes only its matching request. thursday-agent receives it
through the existing loopback connection without waiting for `session.idle` or
a final chat message. Repeating the same reply is safe. Replacing an existing
answer or replying to an unknown request fails.

`thursday_send_prompt` returns a delivery receipt immediately, not an answer.
Use its `requestId` input to read a receipt. Requests use immediate delivery,
not the normal deferred queue. Each request has its own ID; an older pending
request does not reject a new one.

The external worker waits up to three minutes for its correlated answer.
Unanswered requests expire and are removed from the app's pending queue where
possible. A timeout is not evidence that the app is still doing work.

Receipts are stored in the owning session's artifact directory and survive a
canvas reload. Pending requests are not replayed after an extension restart;
only this extension's old queued messages are removed. The authenticated bridge
binds to loopback only. Its private descriptor is passed only to the thursday-agent
process started by the canvas.

</details>

## Mac and browser control

thursday-agent uses Peekaboo for Mac apps and browser windows. Both workers and the
operator helper use the current browser, or a browser you request. They inspect
the visible page before interaction and verify clicks and text input afterward.
The [Mac permissions](#mac-permissions) above are required for these controls.

No browser-specific setup is required.

One-shot shell commands stop their shell and child processes on timeout or
cancellation. A failed command does not speak its success message.

### Window effects

The native Swift and Metal helper draws the glow and window highlights.
The glow reads the selected window's bounds immediately before each rendered
frame. Moves and resizes follow those bounds without position smoothing.
Switching focus to another window still uses a short, smooth transition.
Window selection and stacking checks run separately at a lower rate.

When a short note is needed to draw attention to a screen area, the workers use
the native macOS Stickies app through Peekaboo. They create a new note beside the
relevant area without covering it or changing existing notes. Stickies notes
remain until closed; `clear_annotations` removes only thursday-agent's window
highlights. Stickies does not require the native helper.

Rebuild the effects with `scripts/build-helper.sh`, then restart thursday-agent.
If compilation fails, the existing helper bundle remains available.
Use `--no-helper` to run without them.

## Development

Build both the Rust binary and native helper:

```sh
scripts/build.sh
```

Build only Rust, or run the checks separately:

```sh
cargo build --release --target-dir target
cargo test
bash scripts/test-helper.sh
node --test .github/extensions/thursday/extension.test.mjs
```

## Sharing the source

Keep credentials, account selections, logs, and session data outside the source
folder. The default configuration and session storage already use locations
outside this folder. If you set `THURSDAY_CONFIG`, use a private path outside the
checkout.

The `.gitignore` excludes build outputs, local `.env` files, root-level
`config.json` and `thursday-config.json` files, bridge descriptors, request
history, logs, and macOS metadata. Files named `.env.example` or
`.env.<provider>.example` remain eligible for publication; use placeholders only.
thursday-agent does not load `.env` files automatically.

Publish only reviewed source files, not a ZIP of the complete working folder.
Ignore rules do not remove files that are already tracked by Git. Inspect the
file list before committing or creating a source archive.

## License

Copyright (c) 2026 Aymen Furter. thursday-agent is available under the
[MIT License](LICENSE).

The Azure and OpenAI canvas marks come from
[Simple Icons 11.15.0](https://github.com/simple-icons/simple-icons/tree/11.15.0)
under [CC0-1.0](https://github.com/simple-icons/simple-icons/blob/11.15.0/LICENSE.md).
Third-party dependencies retain their own licenses. Product names and logos
remain the property of their respective owners; the MIT license does not grant
trademark rights or imply endorsement.
