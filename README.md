# Codex App Server Minimal

This repository is a minimal local project for verifying that `codex app-server` can power both:

1. a one-shot CLI harness
2. a tiny local web UI

The bridge is intentionally simple:

- `codex app-server` runs locally over `stdio`
- a Node bridge speaks JSON-RPC to it
- the web page talks only to the Node bridge
- streamed app-server notifications are forwarded to the browser over SSE

Official references used while building this:

- [Codex App Server](https://developers.openai.com/codex/app-server/)
- [Getting started](https://developers.openai.com/codex/app-server/#getting-started)
- [Message schema](https://developers.openai.com/codex/app-server/#message-schema)
- [Approvals](https://developers.openai.com/codex/app-server/#approvals)
- [Events / Items](https://developers.openai.com/codex/app-server/#items)
- [Item deltas](https://developers.openai.com/codex/app-server/#item-deltas)

## What is in here

- `src/codex-app-server.mjs`: reusable bridge for `initialize`, `account/read`, `model/list`, `thread/start`, `turn/start`, and notification handling
- `src/server.mjs`: local HTTP server plus SSE stream for the browser
- `src/cli.mjs`: one-shot CLI smoke test
- `public/index.html`: minimal browser UI
- `public/app.js`: browser behavior
- `public/styles.css`: intentionally simple UI styling

## Prerequisites

- Node.js 22 or newer
- `codex` installed and available on `PATH`
- a working local Codex login if your provider requires OpenAI auth

Checked in this environment on `2026-04-07` with:

- `codex-cli 0.118.0`
- `node v22.22.0`

## How to use

### Web UI

Start the local web server:

```bash
npm start
```

Then open:

```text
http://127.0.0.1:3000
```

What to do in the page:

1. Wait until the session status shows `ready`.
2. Confirm the account and selected model look correct.
3. Type a prompt in the textarea.
4. Click `Send`.
5. Watch the transcript and recent events update in real time.
6. If you want a clean conversation, click `New thread`.

Good first prompts:

- `Reply with exactly the word ready.`
- `Summarize the current repository in 3 bullets.`
- `What model are you currently using?`

### CLI smoke test

Run the old one-shot harness:

```bash
npm run cli
```

Or with a custom prompt:

```bash
npm run cli -- "Reply with exactly the single word ready."
```

## Important behavior in this minimal demo

This web UI is intentionally conservative.

- It supports normal prompt -> thread -> turn flows.
- It streams state updates and recent notifications to the browser.
- It does **not** implement interactive approval UI.
- If `codex app-server` sends approval requests for command execution or file changes, this demo automatically responds with `decline` and shows that in the transcript/events.
- Any unsupported server-initiated request is rejected with a JSON-RPC error and surfaced in the UI.

That tradeoff keeps the demo safe and small while still proving the integration path.

## What the web app proves

If the page works locally, you have confirmed that:

- your own process can spawn `codex app-server`
- the JSON-RPC handshake works
- auth state and model discovery work through the protocol
- a browser can control the bridge without talking to Codex directly
- streamed notifications can be forwarded into a real UI
- thread creation and multi-turn prompting work from the browser

## Observed on this machine

A real CLI run in this repository on `2026-04-07` completed successfully and returned `pong`.

Non-blocking warnings also appeared in this environment:

- a local skill file outside this repository had invalid YAML
- featured plugin cache warmup returned `403 Forbidden`

Neither warning blocked `initialize`, `account/read`, `model/list`, `thread/start`, `turn/start`, or the final `turn/completed` event.

## Troubleshooting

- If the page never reaches `ready`, check the terminal where `npm start` is running.
- If `account.summary` is `none` and `requiresOpenaiAuth=true`, sign in to Codex first.
- If prompts that require shell commands or file writes seem to stop, check the transcript. This demo auto-declines those approval requests by design.
- If port `3000` is busy, run `PORT=3001 npm start` and open the matching URL.

## Next step

Once this is stable, the next sensible iteration is one of:

- add a real approval UI for command/file-change requests
- support multiple browser sessions instead of one shared bridge
- switch the backend transport from `stdio` to the experimental WebSocket mode
