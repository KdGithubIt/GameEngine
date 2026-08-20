# Remote AI Studio Deployment

Status: Accepted
Canonical location: `docs/REMOTE_AI_STUDIO_DEPLOYMENT.md`
Implements: ADR 0133 slice 4 (private-network deployment)

## Purpose

This document is the reference deployment for reaching Remote AI Studio from a
phone or another personal device. ADR 0133 owns the decisions; this document
owns the concrete topology, the setup steps, and the checks that prove the
deployment is the one the ADR describes.

Remote AI Studio is a companion surface over the same project agent host: a
conversation, a proposal, **Go**, **Stop**, permission answers, semantic
progress, validation and playtest results, and captured frames. It is not a
remote Editor and not a second project writer.

## Topology

```text
iPhone / remote personal device
  -> authenticated private overlay network (device identity, not a password)
  -> private HTTPS reverse proxy on the host PC
  -> 127.0.0.1 Remote AI Studio Gateway (Editor process)
  -> Agent Host
  -> loopback-only Editor MCP endpoint
  -> live Editor authoring services
```

Two properties of this diagram are load-bearing:

- The gateway binds only to loopback. GameEngine never binds it to a LAN
  address, a public address, or a router-forwarded port.
- The Editor MCP endpoint is never the remote API. A remote client can take AI
  Studio user actions; it cannot speak the authoring protocol, and MCP
  endpoints, ports, and bearer material are never sent to the browser.

Reachability from outside the house is therefore provided by something else: an
authenticated private overlay network plus a private reverse proxy that
terminates TLS and forwards to loopback. The reference deployment below uses
Tailscale and Tailscale Serve, but nothing in GameEngine depends on Tailscale's
API, address format, or account model. Any overlay with authenticated device
identity and a loopback-targeting reverse proxy satisfies the same contract.

## Host setup

1. Open the project in the Editor and start the Remote AI Studio gateway from
   AI Studio. The listening address is always on `127.0.0.1`. It is shown under
   **Settings → Remote → Advanced**, because it is what the proxy in the next
   steps needs and not an address a phone can open: `127.0.0.1` names whatever
   device reads it.
2. Install the private overlay client on the host PC and sign in with the
   personal account that owns the devices. Confirm the host appears in the
   overlay's device list.
3. Publish the loopback gateway to the overlay, and only to the overlay. With
   Tailscale Serve that is one command, where `PORT` is the gateway port shown
   under **Settings → Remote → Advanced**:

   ```text
   tailscale serve --bg --https=443 http://127.0.0.1:PORT
   ```

   The equivalent for another stack is a reverse proxy that listens on the
   overlay interface, terminates TLS with a certificate the overlay issues, and
   proxies to `http://127.0.0.1:PORT`. Publish it at the **root** of the origin:
   the companion fetches its API from `/api/...`, so a path prefix does not
   work, and AI Studio rejects a base URL that carries one.
4. Enter the origin the proxy publishes — for example
   `https://my-pc.tailnet-name.ts.net` — in **Settings → Remote**, under
   *Address your private network publishes for this PC*. AI Studio composes the
   phone URL from that origin and the gateway's own access token, and reports
   *URL ready*. Nothing detects this origin automatically: ADR 0133 §4 keeps
   GameEngine independent of any particular overlay, so the hop you own is the
   hop you name.
5. Use **Copy phone URL** and send the result to your own device. Treat it as a
   credential: it authorizes the session on top of the overlay identity. The
   displayed form masks the token; only the copied form carries it, and a new
   token is issued each time the Editor starts.
6. Do **not** enable any funnel, tunnel, or public-ingress mode. Public ingress
   is outside the supported deployment: it needs a threat model, an
   authentication design, abuse controls, and its own decision record.

## Device setup

1. Install the same overlay client on the phone and sign in with the same
   personal account.
2. Open the copied phone URL in the phone browser. The overlay identity is what
   authorizes the connection; the gateway access token in the URL fragment
   authorizes the session.
3. Add the page to the home screen if you want it to behave like an app. The
   companion is a responsive web client by design; no native app is required.
4. The companion presents the same three selections as the PC — mode, AI, and
   effort — over the same values, so a change made on either side is what the
   other shows (ADR 0164 §5, §6). It selects only: registering a model, signing
   an agent in, entering a credential, and the remote address itself all stay on
   the machine that owns them.

## Operating notes

- **The host must already be online with the project open.** Remote AI Studio
  does not start the machine, launch the Editor, or open a project. A remote
  request arriving with no active project session is refused, not queued.
- **Runs outlive the connection.** Closing the browser, locking the phone, or
  moving between Wi-Fi and cellular does not cancel an active run. Reconnecting
  restores the authoritative snapshot and resumes the ordered event cursor.
- **Retries are idempotent.** Go, Stop, permission answers, and awaiting-user
  answers carry a request identity, so a retry after a dropped connection does
  not start a second run or apply a decision twice.
- **Captured frames are the built-in visual result.** Live desktop viewing, when
  wanted, is an external remote-display product used over the same private
  overlay; it is not part of the gateway.

## Validation checklist

Run these on the deployment, not only in tests:

| Check | Expected |
| --- | --- |
| `netstat` for the gateway port on the host | bound to `127.0.0.1` only |
| Remote section with no address entered | reports not ready, and offers no loopback URL as a substitute |
| Phone URL from the host browser | loads over the overlay hostname |
| Phone URL from the phone on cellular, overlay connected | loads |
| Phone URL with the overlay client signed out or disabled | fails to reach the host |
| Change the AI on the phone | the PC composer shows the same AI |
| Editor MCP port from the phone | not reachable |
| Start a run, close the browser, reopen after a minute | run still active, timeline resumes without gaps |
| Answer a pending permission twice (retry the request) | one recorded decision |
| Captured frame from another project session | not retrievable |

The last four rows have automated coverage at the gateway layer; running them
against the real deployment is what proves the network path, which tests cannot
observe.

## Failure diagnosis

| Symptom | Likely cause |
| --- | --- |
| Phone cannot resolve the host name | overlay client signed out on either device |
| Name resolves, connection refused | reverse proxy not published, or Editor gateway stopped |
| Page loads, session list empty | Editor has no project open, or a different project is open |
| Actions rejected as stale | proposal advanced on the host; reload the companion |
| Frames missing | run captured no frame yet; frame capture is host-owned and gated |
