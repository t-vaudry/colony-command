# Remote access

Colony Command can let a phone or tablet watch your bots, and, if you allow it,
answer approvals and type replies, over a VPN you already run
(Tailscale or WireGuard). Nothing is reachable from outside your own VPN, and
remote access is **off by default**.

This page covers setup, what a paired device can and cannot do, and how to
revoke one. The mobile app itself is a later milestone; the daemon side
described here is what it will connect to.

## How it works

colonyd normally listens on `127.0.0.1` only. With remote access on it also
listens on your VPN address, and serves **only** `/ws` (the live map),
`/api/agents` (the snapshot) and `/api/pair` (pairing). The routes the hooks
use (`/api/permission`, `/api/ack`, `/api/ingest`, `/api/shutdown`) are not
served there. It never listens on `0.0.0.0`, and refuses a wildcard, loopback,
multicast or broadcast address.

Traffic is plain `ws://`, because the VPN already encrypts and authenticates
it. A device's credential is never put in a URL: it is sent in the
`Authorization` header, or, from a webview that can't set headers on a
WebSocket, as the socket's first message. A socket that doesn't identify itself
within 5 seconds is closed.

## Set up

1. **Install Tailscale** (or set up WireGuard) on the PC and on the tablet, and
   sign both into the same tailnet. Check that the tablet can reach the PC.
   With Tailscale, `tailscale ip -4` on the PC prints its address (`100.x.y.z`).
2. **Tell Colony which address to listen on.** Add this to
   `%USERPROFILE%\.colony\settings.json` (create the file if it isn't there):

       { "remote_listen": "100.101.102.103" }

   or set the `COLONY_REMOTE_ADDR` environment variable (it wins over the
   file). `COLONY_REMOTE_PORT` changes the port; the default is the same as the
   local one (7878). Use your VPN interface's own address, never `0.0.0.0`.
3. **Restart colonyd.** Quit Colony Command from the tray and end `colonyd.exe`
   in Task Manager (your sessions live in `colony-ptyd` and carry on), then start
   Colony Command again. The side panel shows **Remote access: on at
   100.x.y.z:7878** when it worked. If it says off, the log says why (for
   example, the address isn't one of this PC's).
4. **Firewall.** The installer adds a Windows Firewall rule, "Colony Command
   remote", that allows inbound connections to `colonyd.exe` only from
   Tailscale's range (`100.64.0.0/10` and `fd7a:115c:a1e0::/48`) and removes it
   on uninstall. With another VPN, add a rule for its range yourself; until
   then the address binds but stays blocked.
5. **Pair the device.** In the desktop app's side panel, under **Paired
   devices**, click **Pair a device**. An 8-digit code appears (valid for
   5 minutes, usable once). On the tablet, enter it; the device sends it to
   `POST http://<pc-address>:7878/api/pair` with `{"code": "...", "name":
   "My tablet"}` and gets back its own credential. The new device shows up in
   the list.

New devices are **view-only**. To let one act, click its **View only** button
to switch it to **Control**. You can switch it back at any time, and it applies
to connections that are already open.

Wrong codes are rate limited: five wrong tries in a minute lock pairing for
that minute and discard the code on screen, so show a new one afterwards.

## What a paired device can do

| | View-only | Control |
|---|---|---|
| See bots, their states, spend and the live terminal output | yes | yes |
| Answer approvals (Allow, Deny, Allow always...) | no | yes |
| Type into, reply to, interrupt, kill, pause or resume a session | no | yes |
| Start a session, change crews or workflows, change settings and budgets | no | yes |
| Pair, list or revoke devices, or change a device's scope | no | no |
| Turn **Pause approvals gating** on or off | no | no |

Notes:

- A device's terminal resize is ignored, so the tablet can't change the size of
  your desktop terminal.
- The tablet only counts as "watching" for approvals while it says it is in the
  foreground. If it goes quiet, requests stop waiting for it and Claude Code's
  own prompt takes over, the same as with no map open.
- A request is answered once: if you answer on the desktop, it clears on the tablet.
- The pause switch is a tray item on the PC and is not reachable remotely at all.

## Threat model

What the VPN gives you: only machines on your tailnet can reach the listener
at all, and the firewall rule narrows that to Tailscale's addresses.

What a **view-only** device can learn: everything the map shows. That includes
bot names, project folders, state, spend, and the text of a bot's last
messages and its terminal output. Terminals can contain anything a command
printed, including secrets. It cannot change anything.

What a **control** device can do: the same as sitting at the map. It can approve
a tool call (which can run commands on your PC), type into any session Colony
started, and start new sessions. Grant control only to devices you would trust
with your keyboard.

Risks to weigh:

- **A stolen or lost device.** Its credential works until you revoke it, even
  after the daemon restarts or Colony updates. If it's unlocked, whoever holds
  it has the device's scope. **Revoke it as soon as you notice** (below), and
  prefer view-only for devices that travel.
- **A compromised tailnet.** Anyone who can reach your PC's VPN address can try
  to pair, but needs a live code from your screen, gets five tries a minute, and
  gets nothing from a wrong one. Leave the pairing panel closed when you aren't
  pairing; codes expire after 5 minutes either way.
- **Credentials at rest.** The PC keeps only a hash of each credential
  (`~/.colony/devices.json`), so reading that file doesn't let anyone connect.
  The device itself holds the credential, so protect it with the device's lock
  screen.
- **Cleartext inside the tunnel.** The connection is `ws://` inside the VPN. The
  tunnel is encrypted; the leg inside your own PC and tablet is not an exposure
  to the network. Don't point `remote_listen` at an interface that isn't a VPN.

## Revoke a device

In the side panel under **Paired devices**, click **Revoke** next to it. The
credential stops working at once and any open connection from it is closed. To
cut off everything, delete `%USERPROFILE%\.colony\devices.json` while colonyd is
stopped, or clear `remote_listen` (and `COLONY_REMOTE_ADDR`) and restart colonyd
to turn the listener off.

## See what was done

Click **Remote activity** under Paired devices. Every command a remote device
sends is recorded in `~/.colony/audit.jsonl` with the device, the kind of
command and the time, and whether its scope allowed it. Like `events.jsonl`, it
never contains prompt text, command text or file paths. Presence heartbeats are
not listed.
