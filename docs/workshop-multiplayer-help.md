# Workshop multiplayer help

## How it works

When you click **Connect** in the Multiplayer tab, Hebnix starts a small
helper program called `hebnix-tc`. It tells the Hebnix server
(`mp.hebnix.com`) that you're online and gets a list of everyone else who is.
Then it connects straight to each of those players. If a direct connection
isn't possible, it goes through a relay server instead.

Rocket League sees those players as if they were on your home network, so
their matches show up under **Local Matches**. You don't need a VPN, admin
rights or any special settings, and Rocket League's normal online play keeps
working while you're connected.

Everyone who is connected can see your LAN match. If you want it private, set
a match password in Rocket League.

Jump to your problem:

- [Connect fails or never finishes](#connect-fails-or-never-finishes)
- [I use a VPN or proxy app](#i-use-a-vpn-or-proxy-app)
- [It says nobody else is online](#it-says-nobody-else-is-online)
- [A player says "can't reach them right now"](#a-player-says-cant-reach-them-right-now)
- [My ping is high](#my-ping-is-high)
- [We're connected but the match doesn't show up](#were-connected-but-the-match-doesnt-show-up)
- [Map downloads from other players don't work](#map-downloads-from-other-players-dont-work)
- [Still stuck?](#still-stuck)

## Connect fails or never finishes

If you see "the multiplayer helper did not start in time" or "Could not start
Workshop multiplayer", the helper couldn't reach the Hebnix server.

1. Check that your internet works.
2. If you run a VPN or proxy app, read the next section. It's the most common
   cause.
3. Try again in a minute. If the Hebnix server is down for a moment, nobody
   can connect, but players who were already connected stay connected.

If it says the helper can't be found, reinstall Hebnix. The helper ships with
it (`lib/hebnix/hebnix-tc` on Linux, `hebnix-tc.exe` next to `hebnix.exe` on
Windows).

## I use a VPN or proxy app

Most VPNs work fine. The trouble usually comes from apps that take over all
traffic and hand out fake addresses for websites. Hebnix then talks to an
address that doesn't exist and gives up.

### Clash, Mihomo, Clash Verge, FlClash, sing-box, v2rayN, Hiddify, Throne

These are usually in **TUN mode** with **fake-IP DNS** (you'll see addresses
like `198.18.x.x`). Let Hebnix's traffic go direct. In your config (or the
app's DNS and rules settings), add:

```yaml
dns:
  fake-ip-filter:
    - "mp.hebnix.com"
    - "tailcat.dev"
    - "+.ipn.dev"

rules:
  # put these at the top of your rules
  - DOMAIN,mp.hebnix.com,DIRECT
  - DOMAIN,tailcat.dev,DIRECT
  - DOMAIN-SUFFIX,ipn.dev,DIRECT
  - PROCESS-NAME,hebnix-tc,DIRECT
  - PROCESS-NAME,hebnix-tc.exe,DIRECT
```

If your rules send direct traffic to a group with its own name (something like
`NO VPN`), use that name instead of `DIRECT`. Restart the app, then click
**Disconnect** and **Connect** in Hebnix.

### Mullvad, Proton VPN, NordVPN, Windscribe and similar

- Turn on **split tunneling** and leave out `hebnix-tc` (on Windows,
  `hebnix-tc.exe`).
- If the VPN has a **kill switch** or "block connections without VPN", turn it
  off while you play.
- Quick test: turn the VPN off and try again. If it works then, it's a VPN
  setting.

### Your internet blocks direct connections

In some countries, or on some school and work networks, direct connections
are blocked. Hebnix then uses relays, which works but adds ping. If even that
fails, try another network, like your phone's hotspot, to check.

## It says nobody else is online

"No other players are on right now" means the Hebnix server has nobody else on
the list. Make sure your friend has clicked **Connect** and that the Multiplayer
tab says **Connected** for both of you. It can take up to 20 seconds for a new
player to appear.

## A player says "can't reach them right now"

You both reached the Hebnix server, but your two PCs can't talk to each other,
not even through a relay. Usually a VPN, proxy or firewall on one side is
blocking it.

- Read [I use a VPN or proxy app](#i-use-a-vpn-or-proxy-app) and check both
  PCs.
- **Windows**: if Windows Defender Firewall asks about `hebnix-tc.exe`, click
  **Allow**. Antivirus suites with their own firewall may need the same.
- **Linux**: most firewalls (ufw, firewalld) allow this by default because the
  connection starts from your side. If you have strict outgoing rules, allow
  UDP for `hebnix-tc`.
- Both players click **Disconnect**, then **Connect** again.

## My ping is high

Next to each player Hebnix shows "connected direct" or "connected through a
relay". A relay adds ping because your traffic takes a detour.

- If you turned on **Hide my IP from other players**, everything goes through
  a relay on purpose. Turn it off for lower ping.
- Otherwise a router, VPN or strict network is blocking a direct connection.
  Disconnecting and connecting again sometimes gets a direct one.

## We're connected but the match doesn't show up

You see each other in Hebnix, but **Local Matches** in Rocket League is empty.

- **You both need the same Workshop map in the same map slot.** Hebnix shows
  who has which map, and you can download a missing one from the other player
  there.
- The host has to actually be in the LAN match. Then press **Refresh** in
  Local Matches.
- If Hebnix says Rocket League was started with `-multihome`, remove that from
  your launch options and restart the game. Older versions of Hebnix needed it.
  This one doesn't, and it breaks the connection.
- If the match has a password, the joining player has to type it in.

## Map downloads from other players don't work

Maps can be downloaded from another player only while you're both connected.

- The other player may have turned off **P2P File Sharing** in Settings. Ask
  them to turn it on, or download the map from the Hebnix Workshop instead.
- Maps you imported yourself, rather than ones from the Hebnix Workshop, can
  only come from a player who has them.

## Still stuck?

Open an issue on GitHub or ask in Discord. Include:

- your OS, and any VPN, proxy, firewall or antivirus you use
- the exact error from the Multiplayer tab
- the multiplayer lines from Hebnix's log:
  - Linux: `grep -iE "hebnix-tc|multiplayer|map sync" ~/.config/hebnix/hebnix.log | tail -60`
  - Windows: open `%AppData%\Hebnix\hebnix.log` and copy the lines with
    `hebnix-tc` in them

Hebnix blanks out connection tokens and IP addresses in those lines, so
they're safe to share.
