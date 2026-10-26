# Voice Pipe

Voice Pipe is a voice bridge between a Discord server and a
[Fluxer](https://fluxer.app) community. It was made for self-hosted Fluxer
instances, and it also works with communities on the official instance
(fluxer.app). When a person joins a voice channel on one platform, a bot
joins the linked channel on the other platform and uses the name and
avatar of that person. When people are in both channels, the bots relay
voice in both directions.

## Contents

- [Setup](#setup)
  - [Requirements](#requirements)
  - [Step 1: Create the Discord bots](#step-1-create-the-discord-bots)
  - [Step 2: Create the Fluxer bots](#step-2-create-the-fluxer-bots)
  - [Step 3: Network setup](#step-3-network-setup)
  - [Step 4: Configure](#step-4-configure)
  - [Step 5: Run](#step-5-run)
- [Commands](#commands)
- [How the bridge works](#how-the-bridge-works)
- [Troubleshooting](#troubleshooting)
- [Limits](#limits)
- [Project layout](#project-layout)
- [License](#license)

## Setup

The bridge uses bot accounts on both platforms:

- **One main bot for each platform.** It watches the voice channels and
  answers the chat commands. When there are not enough proxy bots, it
  joins voice as **Multiple Users** and speaks for everyone.
- **Proxy bots (optional, any number for each platform).** A proxy copies
  the name and avatar of one person from the other platform while that
  person is in voice. Make one proxy for each person that you expect on
  the other platform at the same time. A Discord proxy shows a Fluxer
  user, and a Fluxer proxy shows a Discord user. Without proxies, the main
  bot speaks for everyone.

Do the steps below in order. They use the `voice-pipe` binary and
`config.example.toml` from the latest release. On Windows and macOS, or if
you prefer containers, run the bridge with Docker instead (see
[Run with Docker](#run-with-docker)). To build the binary yourself, see
[Build from source](#build-from-source).

### Requirements

- **A machine that stays on** to run the bridge. It needs no open port of
  its own.
- **A Discord server** where you can add bots.
- **A Fluxer community** with voice channels, on fluxer.app or on a
  self-hosted Fluxer instance with voice turned on, where you can add
  bots.

### Step 1: Create the Discord bots

Do these steps for the main bot and for each proxy bot. Each bot is a
separate application.

1. Go to the
   [Discord Developer Portal](https://discord.com/developers/applications)
   and select **New Application**.
2. Open **Bot**, select **Reset Token**, and copy the token.
3. **Main bot only:** under **Privileged Gateway Intents**, turn on
   **Message Content Intent**. Without it, the bot cannot read commands, and
   Discord closes the connection with code 4014.
4. Invite the bot to your server. Replace `CLIENT_ID` with the application
   ID from **General Information**:

   - Main bot (View Channels, Send Messages, Read Message History, Connect,
     Speak, Change Nickname):

     ```
     https://discord.com/oauth2/authorize?client_id=CLIENT_ID&scope=bot&permissions=70323200
     ```

   - Proxy bot (View Channels, Connect, Speak, Change Nickname):

     ```
     https://discord.com/oauth2/authorize?client_id=CLIENT_ID&scope=bot&permissions=70255616
     ```

5. Make sure that the bots can see and join the voice channels that you
   want to link. Channel permission overrides can block them.

You also need the server ID. Turn on **Developer Mode** (User Settings,
Advanced), then right-click the server and select **Copy Server ID**.

### Step 2: Create the Fluxer bots

Do these steps for the main bot and for each proxy bot.

1. Create a bot application and copy its bot token. See the Fluxer
   documentation for
   [applications](https://docs.fluxer.app/http-api/applications/) and for
   [adding a bot to a community](https://docs.fluxer.app/http-api/oauth2/).
2. Add the bot to your community with these permissions:
   - Main bot: view channels, send messages, read message history,
     connect, speak, and change nickname.
   - Proxy bot: view channels, connect, speak, and change nickname.
3. Make sure that the bots can see and join the voice channels that you
   want to link.

You also need the community (guild) ID.

A proxy copies the avatar of a person as its avatar in the community. If
Fluxer does not accept that for a bot, the bridge logs a warning and the
proxy keeps its own avatar. Name copies and voice still work.

### Step 3: Network setup

The bridge only makes outgoing connections, so it needs no open port.

**Discord** needs no setup. The bridge connects to Discord over the
internet, like the Discord app.

**Fluxer** needs the URLs of your Fluxer instance in the `[fluxer]` part of
`config.toml`. Use the part below that matches your instance.

#### Official instance (fluxer.app)

Set these values in the `[fluxer]` part of `config.toml`:

```toml
api_url = "https://api.fluxer.app/v1"
gateway_url = "wss://gateway.fluxer.app/?v=1&encoding=json"
livekit_url = ""
avatar_url = "https://fluxerusercontent.com/avatars/{user_id}/{hash}.png?size=128"
host_header = ""
```

`livekit_url = ""` makes the bridge use the voice server that Fluxer gives
for each call. If avatars do not copy, compare `avatar_url` with the avatar
URLs that the Fluxer web app loads (network tab of the browser developer
tools).

#### Self-hosted Fluxer on the same machine

The defaults in `config.example.toml` are for this setup. The bridge talks
to the Fluxer stack directly on `127.0.0.1:8080` (the default
`FLUXER_EDGE_BIND`). It does not go through a reverse proxy (such as nginx
or Cloudflare) that you have in front of Fluxer. Check these items:

1. **Edge port.** If your stack does not listen on `127.0.0.1:8080`, change
   the address in `api_url`, `gateway_url`, `livekit_url`, and
   `avatar_url`.
2. **Host routing.** If the Fluxer edge answers only for your public host
   name, set `host_header` to that name (for example `chat.example.com`).
   The bridge then sends that host name in its local requests.
3. **Voice ports.** LiveKit sends voice over UDP 7882 (and TCP 7881 as a
   fallback). Your Fluxer users need these ports open from the internet.
   The bridge uses them on the same machine.
4. **LiveKit address.** If the bridge joins Fluxer voice but no audio
   flows, LiveKit probably offers only its public IP address, and your
   router does not send traffic from the machine back to itself (hairpin
   NAT). Let LiveKit also offer its local interface addresses (see the
   LiveKit `rtc` settings in the Fluxer stack), or turn on hairpin NAT on
   the router.

#### Self-hosted Fluxer on another machine

Use the URLs of that server. For a server at `chat.example.com`:

```toml
api_url = "https://chat.example.com/api/v1"
gateway_url = "wss://chat.example.com/gateway?v=1&encoding=json"
livekit_url = ""
avatar_url = "https://chat.example.com/media/avatars/{user_id}/{hash}.png?size=128"
host_header = ""
```

Use the same host names and paths that the Fluxer web app of that server
uses (network tab of the browser developer tools). The bridge must be able
to reach the LiveKit voice ports of that server (UDP 7882 or TCP 7881 by
default).

#### Rules for remote URLs

These rules apply to the official instance and to any other machine:

- **Use `https://` and `wss://`.** The bot tokens go in each request. With
  `http://` or `ws://` to another machine, anybody on the network path can
  read them. The bridge writes a warning at start for each URL like this.
  A plain URL is acceptable only on a network that you trust.
- **Certificates.** The bridge accepts only certificates from a public
  certificate authority (for example Let's Encrypt or Cloudflare). A
  self-signed certificate does not work.
- **Latency.** Each extra network hop adds delay to the relayed audio.

### Step 4: Configure

Download the `voice-pipe` binary and `config.example.toml` from the latest
release, and put them in one directory (for example `/opt/voice-pipe`).
With Docker, put `config.toml` in a `data` directory next to
`docker-compose.yml` instead (see [Run with Docker](#run-with-docker)).
Then make your config from the example:

```sh
cp config.example.toml config.toml
```

Fill in `config.toml`. The comments in the file explain each setting.
These are required:

| Setting | Value |
| :--- | :--- |
| `discord.guild_id` | Discord server ID (step 1) |
| `discord.main_token` | Token of the Discord main bot |
| `fluxer.guild_id` | Fluxer community ID (step 2) |
| `fluxer.main_token` | Token of the Fluxer main bot |
| `fluxer.api_url`, `gateway_url`, `livekit_url`, `avatar_url` | URLs of your Fluxer instance (step 3) |

Optional settings:

| Setting | Default | Use |
| :--- | :--- | :--- |
| `discord.proxy_tokens` | `[]` | Discord proxies (mirror Fluxer users) |
| `fluxer.proxy_tokens` | `[]` | Fluxer proxies (mirror Discord users) |
| `discord.command_role_ids` | `[]` | Discord roles that can use toggle, link, confirm, and unlink |
| `fluxer.command_role_ids` | `[]` | Fluxer roles that can use the same commands (both lists empty = everyone, with a warning at start) |
| `fluxer.host_header` | none | Host name for requests to a local Fluxer (step 3) |
| `general.command_prefix` | `vp!` | Prefix of the chat commands |
| `general.start_enabled` | `true` | Start with the bridge on (when no state is saved) |
| `general.state_file` | `voice-pipe-state.toml` | File that keeps the on/off state (`""` = do not save) |
| `general.match_by_name` | `true` | Link voice channels with the same name |
| `general.ignore_afk_channels` | `true` | Do not link AFK channels by name |
| `general.overflow_name` | `Multiple Users` | Nickname of a group bot |
| `general.proxy_idle_name` | `Voice Pipe Proxy {number}` | Nickname of a free proxy |
| `general.log_level` | `info` | Log filter (see [Logs](#logs)) |
| `general.voice_threshold` | `50` | Level that starts speech, in 16-bit RMS amplitude (50 is about -56 dBFS); `0` sends all audio |
| `general.silence_hangover_ms` | `300` | Send time after speech stops, in milliseconds (minimum 20) |
| `general.jitter_frames` | `3` | Buffer of the group mixer (20 ms frames) |
| `general.audio_reports` | `false` | Log a receive report (packets, loss, levels, stereo) for each Discord speaker every 10 s |
| `[[links]]` | none | Fixed channel pairs (see below) |

The bridge checks the config when it starts and stops with a clear message
if a required value is missing.

**Linked channels.** By default, voice channels with the same name on both
platforms are linked. To link channels with different names, use the
`vp!link` and `vp!confirm` commands (see [Commands](#commands)), or add an
entry with the two channel IDs:

```toml
[[links]]
discord_channel_id = "123456789012345678"
fluxer_channel_id = "987654321098765432"
```

#### Units of the audio settings

**`voice_threshold`** is a root mean square (RMS) amplitude of 16-bit audio
samples, measured over each 20 ms frame. The scale is linear: 0 is digital
silence and 32768 is full scale. To change a level in dBFS (decibels below
full scale) to this unit, use `32768 × 10^(dBFS / 20)`.

| `voice_threshold` | dBFS | Typical sound (approximate) |
| ---: | ---: | :--- |
| 10 | -70 | Very quiet room, noise suppression on |
| 33 | -60 | Quiet room |
| 50 | -56 | Default |
| 100 | -50 | Fan or computer noise |
| 330 | -40 | Quiet speech |
| 1000 | -30 | Normal speech |
| 3300 | -20 | Loud speech |

The level is measured after decoding, so it is the level that the sender's
client sends, after its own gain and noise suppression. With
`audio_reports = true`, the `rms` value of a receive report uses the same
unit.

**`silence_hangover_ms`** is a time in milliseconds. The speech gate counts
it in 20 ms frames and rounds down, so 300 ms is 15 frames and 310 ms is
also 15 frames.

### Step 5: Run

In the directory with the binary and `config.toml`:

```sh
chmod +x voice-pipe
./voice-pipe                        # reads ./config.toml
./voice-pipe /path/to/config.toml   # or give the path of the config
```

Stop the bridge with Ctrl+C or `SIGTERM`. The bots leave voice and log out
before the process stops (10 s limit).

The bridge writes two files: the on/off state (`state_file`, in the
working directory by default), and `config.toml` when the link commands
change it. The user that runs the bridge must be able to write to
`config.toml` and to the directories of both files.

#### Logs

```sh
RUST_LOG=info,voice_pipe=debug ./voice-pipe
```

With `log_level` from the config, the voice libraries (Songbird, LiveKit,
WebRTC) log only warnings, because Songbird writes several lines each time
a person starts or stops speaking. `RUST_LOG` replaces the whole filter,
so it also shows the library logs.

#### Run as a systemd service

This example uses `/opt/voice-pipe` for the binary and `config.toml`, and
a `voice-pipe` user that owns that directory.
`/etc/systemd/system/voice-pipe.service`:

```ini
[Unit]
Description=voice-pipe Discord and Fluxer voice bridge
After=network-online.target
Wants=network-online.target

[Service]
WorkingDirectory=/opt/voice-pipe
ExecStart=/opt/voice-pipe/voice-pipe /opt/voice-pipe/config.toml
Restart=on-failure
RestartSec=10
User=voice-pipe
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
```

If Fluxer runs on the same machine in Docker, add `docker.service` to
`After=`, so that the bridge starts after Fluxer.

`LimitNOFILE` is necessary. Each Fluxer bot holds WebRTC connections with
many sockets, and each Discord bot holds its own connections. The systemd
default of 1024 open files is too low for a few bots, and joins then fail
with "Too many open files".

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now voice-pipe
journalctl -u voice-pipe -f
```

#### Run with Docker

Docker runs the Linux build on Linux, Windows, and macOS (with Docker
Desktop). `docker-compose.yml` in the base directory of the project builds
the image from the source on your machine and runs it.

1. Get the source of the latest release, and open its directory.
2. Make a `data` directory there and put your `config.toml` in it (step
   4). Make the directory yourself before the first start: if Docker makes
   it, it belongs to root, and the bridge cannot write to it.

   ```sh
   mkdir data
   cp config.example.toml data/config.toml    # then fill it in
   ```

3. **Linux only.** On Windows and macOS, Docker Desktop handles the file
   owners, so skip this step.

   Tell Docker which user to run the bridge as. The bridge must be able to
   write to `data`, because the link commands change `config.toml` and the
   bridge saves its on/off state there. Copy `.env.example` to `.env`, and
   set `PUID` and `PGID` to your user and group IDs:

   ```sh
   cp .env.example .env
   id -u    # your user ID: put it in PUID
   id -g    # your group ID: put it in PGID
   ```

   `docker-compose.yml` reads `PUID` and `PGID` from `.env` (the default is
   1000 for both). The files that the bridge writes then belong to you.

4. Start the bridge. The first build downloads the WebRTC library and takes
   a few minutes.

   ```sh
   docker compose up -d
   docker compose logs -f      # watch the logs
   docker compose down         # stop (the bots leave voice first)
   ```

The Fluxer URLs in `config.toml` depend on where Fluxer runs:

- **Official instance or another machine:** the remote URLs from step 3.
  No change to `docker-compose.yml`.
- **Self-hosted Fluxer in Docker on the same machine:** inside the
  container, `127.0.0.1` is the container itself. Connect voice-pipe to the
  Docker network of Fluxer: uncomment the two `networks` parts of
  `docker-compose.yml`, and set the network name (`docker network ls`
  shows it). Then use the name of the Fluxer edge container in the URLs,
  for example `http://<edge-container>:8080/api/v1`. Set `host_header` if
  the edge routes requests by host name.
- **Self-hosted Fluxer not in Docker, on the same Linux machine:**
  uncomment `network_mode: host` in `docker-compose.yml`. The default
  `127.0.0.1` URLs then work. This is not available with Docker Desktop.

Mount the whole `data` directory, as the compose file does, and not only
`config.toml`. The link commands replace `config.toml` with a rename, and
a rename over a single mounted file fails in a container.

#### Build from source

To build the binary yourself instead of using the latest release, you
need:

- **Rust 1.88 or newer, with Cargo** (the Rust build tool). The easiest way
  to get both is [rustup](https://rustup.rs). The Rust packages of some
  distributions are older than 1.88; check with `cargo --version`.
- **A C/C++ compiler and CMake.** The Discord voice library builds libopus.
  The LiveKit library (Fluxer voice) downloads a prebuilt WebRTC library on
  the first build, so the first build needs internet access and takes a
  few minutes.

Arch Linux (the `rust` package includes Cargo):

```sh
sudo pacman -S --needed rust base-devel cmake clang pkgconf
```

Debian or Ubuntu (install Rust and Cargo with rustup, because the Rust
packages of these distributions can be older than 1.88):

```sh
sudo apt install build-essential cmake clang pkg-config
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Then, in the source directory:

```sh
cargo build --release
```

The binary is `target/release/voice-pipe`, and `config.example.toml` is in
the source directory. Use them as in steps 4 and 5.

The build uses a patched copy of Songbird 0.6.1 (the Discord voice
library) in `vendor/songbird`. Songbird 0.6.1 gives the DAVE encryption
trailer to the Opus decoder with the audio, which damages the audio of
Discord users whose client sends CELT (for example, some clients with a
webcam on). The patch is in `vendor/songbird-dave-trailer.patch`. If a
fix for this in Songbird is released, the patch will be removed.

## Commands

Both main bots answer the commands, on Discord and on Fluxer. Send `help`,
`toggle`, and `status` in any text channel that the main bot can read.
Send `link`, `confirm`, and `unlink` in the text chat of a voice channel:
the bridge uses that voice channel. The examples use the default prefix
`vp!`.

| Command | Result |
| :--- | :--- |
| `vp!help` | Shows the list of commands. |
| `vp!toggle` | Turns the bridge off or on. When it is off, all bots leave voice and the bridge ignores users. Needs a command role. |
| `vp!status` | Shows whether the bridge is on, the free proxies, where the main bots are, and each active link: users on each side, mirrors, and seconds of relayed speech. |
| `vp!link` | Starts to link this voice channel. The bot answers "Listening to `vp!confirm`". The request ends after 5 minutes, or when someone sends a new `vp!link`. Needs a command role. |
| `vp!confirm` | In a voice channel on the other platform: links this channel with the channel of the request. The link starts at once and is saved as a `[[links]]` entry in `config.toml`. Needs a command role. |
| `vp!unlink` | Removes the fixed link of this voice channel, from the bridge and from `config.toml`. Needs a command role. |

To link a Discord channel with a Fluxer channel: send `vp!link` in the text
chat of one of them, then `vp!confirm` in the text chat of the other within
5 minutes.

**Command roles.** `toggle`, `link`, `confirm`, and `unlink` need a command
role. The command roles are the roles in `discord.command_role_ids` and in
`fluxer.command_role_ids` together: a user on either platform with a role
from either list can use these commands. When both lists are empty,
everyone can use all commands (the bridge logs a warning at start). `help`
and `status` are open to everyone. If only one list is set, users on the
other platform cannot use these commands, because they have none of the
roles.

**Config file changes.** `confirm` appends a `[[links]]` entry at the end of
`config.toml`, and `unlink` removes only the entry of that link. All other
lines and comments stay the same. The bridge checks that the changed file is
a valid config before it writes it, and keeps the file permissions. A
channel that is already in a fixed link cannot be linked again: send
`vp!unlink` first. `unlink` cannot remove a link by name; rename a channel
or set `match_by_name = false` for that.

**On/off state.** `toggle` saves the state in `state_file`, so it stays the
same after a restart. `start_enabled` is used only when there is no saved
state.

Each user can send one command every 3 seconds. The bridge ignores other
commands from that user during this time, so that command spam does not
cause API rate limits.

Example status:

```
voice-pipe status
Bridge: on
Discord proxies free: 1 of 2 · main bot: not in voice
Fluxer proxies free: 1 of 2 (1 wait after a failure) · main bot: group bot in General
Active links: 1

General · audio relay on
Discord users: 3 · Fluxer users: 1
In Discord: Bob
In Fluxer: Multiple Users (3)
Relayed speech: 41.2 s to Discord, 63.8 s to Fluxer
```

## How the bridge works

### Words used in this README

| Word | Meaning |
| :--- | :--- |
| Link | A Discord voice channel and a Fluxer voice channel that the bridge joins together. |
| Mirror | The bot that shows a person in the linked channel on the other platform. |
| Proxy | A bot that mirrors one person, with that person's name and avatar. |
| Group bot | One bot that mirrors all the people of a channel as **Multiple Users**, when there are not enough proxies. |
| Listener | The one bridge bot in each channel that receives the audio of that channel for the bridge. |

### Single proxies and group bots

The mirrors in a channel work in one of two modes:

- **Single proxies.** When there is a free proxy for each person, each
  person gets their own proxy.
- **Group bot.** When there are not enough proxies, one bot speaks for all
  the people with the nickname **Multiple Users**. This is the main bot. If
  the main bot is already a group bot in another link, a proxy becomes the
  group bot. When a proxy of the channel is already in voice, it only
  changes its name, so it does not leave and join again. When a group bot
  is in voice, all the single proxies of that channel leave, so that other
  links can use them.

A channel goes back to single proxies when enough proxies are free for all
of its people for 10 seconds. The new proxies join first, and the group bot
leaves only after they are all in voice, so no speech is lost. A proxy that
was the group bot stays in voice and becomes the proxy of the last person.

A proxy also shows when its person is muted or deafened. A group bot speaks
for many people, so it never shows a mute or a deafen. A deafened bot gets
no audio, so the bridge prefers a proxy whose person is not deafened as the
listener. When all the people on the other platform are deafened, nobody
can hear the channel, so a deafened listener loses nothing.

### Linked channels

The bridge finds links in two ways:

1. Fixed links from the `[[links]]` entries in `config.toml`.
2. Channels with the same name on both platforms (when `match_by_name` is
   `true`). Outer spaces are ignored, but names are case-sensitive (for
   example, `General` matches ` General `, but does not match `general`).
   When `ignore_afk_channels` is `true`, the AFK channel of each server is
   not linked by name. A `[[links]]` entry can still link it.

The bridge can run many links at the same time. The proxy pool is shared
between all links. The main bot can be in only one voice channel for each
server, so only one link at a time can use it as a group bot. Other links
use a proxy as their group bot.

### Presence and audio

```
        Discord channel "General"            Fluxer channel "General"
        ─────────────────────────            ────────────────────────
        Alice (person)  ──── mirrored as ──▶ proxy "Alice"
        Multiple Users  ◀── mirrored as ──── Bob, Carol, Dave
                                             (not enough proxies)
```

1. **One side has people.** For each person, the bridge puts a mirror in
   the linked channel on the other platform: a proxy with the person's
   name and avatar, or one group bot as "Multiple Users". People on the
   other platform can see who is in the channel before they join. **No
   audio is relayed in this state.**
2. **Both sides have people.** On each side, one bridge bot becomes the
   listener. The bridge gives the audio of each speaker to the mirror of
   that speaker on the other platform. For a group, the speakers are mixed
   together and the group bot plays the mix.
3. **A person leaves.** Their mirror leaves, and the proxy gets its idle
   nickname again (`Voice Pipe Proxy 1`, `Voice Pipe Proxy 2`, ...) and
   the default avatar of its bot account. When both channels are empty,
   the link stops and all of its bots leave.

### Audio only when it is necessary

- Bots relay audio only when people are in both channels, and only the
  listener gives its audio to the bridge. On Fluxer the other bots do not
  subscribe to any audio track. On Discord the voice library (Songbird)
  decodes the audio on every bot in voice, but the bridge ignores it on
  all bots except the listener.
- Only voice is relayed. On Fluxer the bot does not subscribe to the audio
  of a screen share.
- A speech gate for each person decides what is sent. A frame louder than
  `voice_threshold` opens the gate, and it stays open until the person is
  quiet for `silence_hangover_ms`. Quiet sounds inside speech are sent, so
  that words are not cut into pieces. Background noise alone does not open
  the gate, so a mirror does not look like an open microphone. Frames of
  digital silence are never sent.
- A mirror sends audio only while its person speaks, plus a short
  `silence_hangover_ms`, so that the ends of words are not cut. Then the
  Discord track pauses and the Fluxer track goes idle (Opus DTX).
- The bridge does not record audio. Audio moves through memory in 20 ms
  frames and is not written to disk.

### Bot statuses

Each bot shows a custom status on both platforms:

| Bot | Bridge not in use | Bridge in use |
| :--- | :--- | :--- |
| Main bot | `Connecting <Discord server> to <Fluxer server>` | `Voice-piping <number of people>` |
| Proxy | `Standing By` | `Proxying <name of the person>` |
| Proxy as group bot | `Standing By` | `Proxying <number of people>` |

The number on the main bots counts the people (not the bots) in all
linked voice channels on both platforms. A status changes at most once in
5 seconds for each bot, because both platforms limit status updates.

### Connections

Each bot has its own gateway connection, and all bots stay logged in while
the bridge runs. A proxy joins voice when it gets a person and leaves voice
when that person leaves, but it stays online. When a connection drops, the
bridge connects again and resumes the session, so voice continues.

A bot that cannot connect tries again on this schedule:

| Time since the connection was lost | Wait between attempts |
| :--- | :--- |
| First minute | 2, 4, 8, 16, then 32 seconds |
| Up to 5 minutes | 1 minute |
| After 5 minutes | 10 minutes |

A bot also tries again after the server refuses its login, for example
when an intent is not turned on in the Discord developer portal. Then you
can correct the setting on the platform while the bridge runs. Each refused
login writes an error to the log. A new token in `config.toml` needs a
restart of the bridge. If the bridge cannot start (for example, Discord
cannot be reached), it starts again on the same schedule.

## Troubleshooting

| Problem | Cause and fix |
| :--- | :--- |
| `close code 4014` | Turn on the Message Content intent for the Discord main bot (step 1). |
| `close code 4004` | A token is not correct. Copy it again. |
| `discord login failed` or `fluxer login failed` | The token or `api_url` is wrong, or Fluxer cannot be reached. |
| `did not send guild` | The bot is not in the server, or `guild_id` is wrong. |
| `no answer to the join` | The platform ignored the voice join. Check the Connect permission (not only View Channel) and the user limit of the channel, because bots count toward it. |
| `proxy token does not work` | That proxy token is not correct. The bridge skips it. |
| `is not encrypted and not local, tokens are visible` | A Fluxer URL uses `http://` or `ws://` to another machine. Use `https://` and `wss://` (step 3). |
| `Too many open files` | Raise the open file limit (`LimitNOFILE`, step 5). |
| Proxy joins but has the wrong name | The bot needs the Change Nickname permission. |
| Fluxer avatar does not copy | Compare `avatar_url` with the avatar URLs of the Fluxer web app (step 3). |
| Fluxer voice connects, no audio | Self-hosted: see item 4 in [Self-hosted Fluxer on the same machine](#self-hosted-fluxer-on-the-same-machine). Other instances: the bridge machine must reach the LiveKit voice ports. |
| Background noise relays | Make `voice_threshold` higher, or ask the person to use push-to-talk or the noise suppression of their app. |
| Ends of words are cut | Make `silence_hangover_ms` higher. |
| A mirror always shows as speaking | Make `voice_threshold` higher. |
| Quiet speakers are not relayed | Make `voice_threshold` lower. |
| Channels do not link | The names must be the same on both platforms, or link them with `vp!link` and `vp!confirm`. |
| Commands get no answer | The main bot needs to read and send messages in that channel. On Discord, the Message Content intent must be on. Check `command_prefix`. |

## Limits

- **One main bot for each platform.** The main bot can be in only one
  voice channel for each server. When two links need a group bot at the
  same time, the second link uses a proxy as its group bot. If that link
  has no proxy and no proxy is free, its people have no mirror until a bot
  is free.
- **One proxy for each person.** A link with 5 Fluxer users needs 5 free
  Discord proxies for all of them to show by name. With fewer, all 5 share
  one group bot.
- **Rate limits.** Nicknames and avatars change through the API. The
  bridge sends an avatar change only when the avatar must change: a
  different person, a group, or the default avatar when the proxy is
  free. Discord and Fluxer can still delay many quick changes.
- **Speech start.** After silence, the first 20 to 60 ms of speech can be
  lost while the receivers fill their buffers. This is the cost of sending
  nothing during silence.
- **Fluxer API.** The Fluxer bot and voice API is new. Changes in Fluxer can
  stop parts of the bridge.

## Project layout

```
src/
  main.rs           Start up, logging, and shutdown signals
  config.rs         Load, fill in, and check config.toml
  platform.rs       The Discord and Fluxer platform values
  commands.rs       Parse chat commands and write the help text
  rest.rs           HTTP API: bot IDs, profiles, nicknames, avatars, replies
  retry.rs          Wait times between connection attempts
  audio/            20 ms audio frames, mixer, jitter buffer, speech gate
  gateway/          Gateway connection (log in, resume, voice view, joins)
  voice/            Voice connections: Songbird (Discord), LiveKit (Fluxer)
  bridge/
    mod.rs          Start the bridge again after errors
    hub.rs          State that all links share: main bots, proxy pool, switch
    coordinator.rs  Start and stop links, answer commands
    chat.rs         Chat commands and the link requests
    fixed_links.rs  The [[links]] entries and changes to config.toml
    saved.rs        The saved on/off state
    matching.rs     Find linked channel pairs
    link.rs         One link task: sync mirrors and relay audio
    side.rs         Bots in one channel of a link: proxies, group bot, listener
    proxy.rs        Proxy pool and proxy sessions (copy name and avatar)
    overflow.rs     Mix the speakers of a group for the group bot
    status.rs       Link state and the status report
vendor/
  songbird/         Songbird 0.6.1 with the DAVE trailer fix (ISC License)
Dockerfile          Container image (build stage and small runtime stage)
docker-compose.yml  Run the container with the data directory mounted
.env.example        Docker settings (PUID, PGID): copy to .env
```

Run the tests:

```sh
cargo test
```

## License

voice-pipe is released under the [MIT License](LICENSE).
Copyright (c) 2026 Grimsace.

`vendor/songbird` is Songbird by the Songbird Contributors, released under
the [ISC License](vendor/songbird/LICENSE.md).
