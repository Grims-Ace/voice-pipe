# Voice Pipe

Voice Pipe is a voice bridge between a Discord server and a self-hosted
[Fluxer](https://fluxer.app) community. When a person joins a voice channel
on one platform, a bot joins the linked channel on the other platform and
uses the name and avatar of that person. When people are in both channels,
the bots relay voice in both directions.

## Contents

- [How the bridge works](#how-the-bridge-works)
- [Requirements](#requirements)
- [Set up the Discord bots](#set-up-the-discord-bots)
- [Set up the Fluxer bots](#set-up-the-fluxer-bots)
- [Network setup](#network-setup)
- [Configure](#configure)
- [Build and run](#build-and-run)
- [Commands](#commands)
- [Troubleshooting](#troubleshooting)
- [Limits](#limits)
- [Project layout](#project-layout)

## How the bridge works

### Bots

The bridge uses two types of bot on each platform:

- **Main bot** (one for each platform). It watches the voice channels. The
  Discord main bot also answers the chat commands. When no proxy is free,
  the main bot joins voice with the nickname **Multiple Users** and speaks
  for all the people that have no proxy.
- **Proxy bots** (zero or more for each platform). A proxy copies the
  nickname and avatar of one person from the other platform and joins voice
  as that person. A proxy on Discord shows a Fluxer user. A proxy on Fluxer
  shows a Discord user.

### Linked channels

A link is a Discord voice channel and a Fluxer voice channel that the bridge
joins together. The bridge finds links in two ways:

1. Fixed links from the `[[links]]` entries in `config.toml`.
2. Channels with the same name on both platforms (when `match_by_name` is
   `true`). Outer spaces are ignored, but names are case-sensitive (for
   example, `General` matches ` General `, but does not match `general`).

The bridge can run many links at the same time. The proxy pool is shared
between all links. The main bot can be in only one voice channel for each
server, so only one link at a time can use it for overflow.

### Presence and audio

```
        Discord channel "General"            Fluxer channel "General"
        ─────────────────────────            ────────────────────────
        Alice (person)  ──── mirrored as ──▶ proxy "Alice"
        proxy "Bob"     ◀── mirrored as ──── Bob (person)
        Multiple Users  ◀── mirrored as ──── Carol, Dave (no free proxy)
```

1. **One side has people.** For each person, the bridge puts a mirror in
   the linked channel on the other platform: a proxy with the person's
   name and avatar, or the main bot as "Multiple Users". People on the
   other platform can see who is in the channel before they join. **No
   audio is received or sent in this state.**
2. **Both sides have people.** On each side, one bridge bot becomes the
   *listener*. Only the listener receives audio. The bridge gives the audio
   of each speaker to the mirror of that speaker on the other platform.
   Overflow speakers are mixed together and the main bot plays the mix.
3. **A person leaves.** Their mirror leaves, and the proxy gets its idle
   nickname again (`Voice Pipe Proxy 1`, `Voice Pipe Proxy 2`, ...). When
   both channels are empty, the link stops and all of its bots leave.

### Audio only when it is necessary

- Bots do not decode or subscribe to audio until people are in both
  channels. On Discord the voice driver does not decrypt or decode. On
  Fluxer the bot does not subscribe to any audio track.
- Audio Inputs with a level below `voice_threshold` are silence and are not sent.
- A mirror sends audio only while its person speaks, plus a short
  `silence_hangover_ms`, so that the ends of words are not cut. Then the
  Discord track pauses and the Fluxer track goes idle (Opus DTX).
- The bridge does not record audio. Audio moves through memory in 20 ms
  frames and is not written to disk.

### Fluxer traffic stays local

Fluxer runs on the same machine as the bridge:

| Traffic | Default address |
| :--- | :--- |
| Fluxer API | `http://127.0.0.1:8080/api/v1` |
| Fluxer gateway | `ws://127.0.0.1:8080/gateway` |
| LiveKit signaling | `ws://127.0.0.1:8080/livekit` |
| Avatars | `http://127.0.0.1:8080/media/...` |
| Voice media | LiveKit UDP 7882 or TCP 7881 on this machine |

Discord traffic goes to Discord over the internet as usual.

### Gateway connections

Each bot has its own gateway connection. A main bot keeps its connection
open. A proxy logs in when it gets a person and logs out when that person
leaves. When a connection drops, the bridge connects again and resumes the
session, so voice continues. If a main bot connection cannot recover, the
bridge logs out all bots and starts again, with a delay that increases
after each failure (4 s up to 2 min).

## Requirements

- **Rust** 1.85 or newer (edition 2024).
- **A C/C++ compiler and CMake.** Songbird builds libopus. The LiveKit
  crate downloads a prebuilt WebRTC library on the first build, so the
  first build needs internet access and takes a few minutes.
- **A Fluxer instance** that runs on the same machine, with voice
  (LiveKit) turned on.
- **Bot accounts**: one main bot for each platform, plus as many proxy bots
  as you want. Each proxy can mirror one person at a time.

Arch Linux:

```sh
sudo pacman -S --needed base-devel cmake clang pkgconf
```

Debian or Ubuntu:

```sh
sudo apt install build-essential cmake clang pkg-config
```

## Set up the Discord bots

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

The server ID goes in `discord.guild_id`. To copy it, turn on **Developer
Mode** (User Settings, Advanced), then right-click the server and select
**Copy Server ID**.

## Set up the Fluxer bots

1. In your Fluxer instance, create one bot application for the main bot and
   one for each proxy. Copy the token of each bot.
2. Add each bot to your community. Give the bots permission to view and
   join the voice channels, to speak, and to change their nickname.
3. Put the community (guild) ID in `fluxer.guild_id`.

The Fluxer bot API is new and not fully documented. Nicknames use the same
endpoint as Discord. Avatar copies are best effort: if Fluxer does not
accept a guild avatar for a bot, the bridge logs a warning and continues.

## Network setup

Your server uses Cloudflare and nginx in front of Fluxer. The bridge does
not use that path for Fluxer. Check these items:

1. **Edge port.** The Fluxer stack must listen on `127.0.0.1:8080` (the
   default `FLUXER_EDGE_BIND`). If you changed the port, change `api_url`,
   `gateway_url`, `livekit_url`, and `avatar_url` in `config.toml`.
2. **Host routing.** If the Fluxer edge answers only for your public host
   name, set `host_header` to that name (for example `chat.example.com`).
   The bridge then sends that host name in local requests.
3. **Voice ports.** LiveKit media uses UDP 7882 (and TCP 7881 as a
   fallback). Cloudflare does not carry this traffic, so these ports must
   be open to the internet for your Fluxer users. The bridge connects to
   them on this machine.
4. **LiveKit address.** If the bridge joins Fluxer voice but no audio
   flows, LiveKit is probably offering only its public IP address, and your
   router does not send traffic from the machine back to itself (hairpin
   NAT). Let LiveKit also offer its local interface addresses (see the
   LiveKit `rtc` settings in the Fluxer stack), or turn on hairpin NAT on
   the router.

The bridge needs no public port of its own.

## Configure

```sh
cp config.example.toml config.toml
```

Then fill in `config.toml`. The comments in the file explain each setting.
These are required:

| Setting | Value |
| :--- | :--- |
| `discord.guild_id` | Discord server ID |
| `discord.main_token` | Token of the Discord main bot |
| `fluxer.guild_id` | Fluxer community ID |
| `fluxer.main_token` | Token of the Fluxer main bot |

Optional settings:

| Setting | Default | Use |
| :--- | :--- | :--- |
| `discord.proxy_tokens` | `[]` | Discord proxies (mirror Fluxer users) |
| `fluxer.proxy_tokens` | `[]` | Fluxer proxies (mirror Discord users) |
| `general.command_prefix` | `vp!` | Prefix of the chat commands |
| `general.start_enabled` | `true` | Start with the bridge on |
| `general.match_by_name` | `true` | Link channels with the same name |
| `general.overflow_name` | `Multiple Users` | Nickname of the overflow bot |
| `general.proxy_idle_name` | `Voice Pipe Proxy {number}` | Nickname of a free proxy |
| `general.voice_threshold` | `200` | Level below which audio is silence |
| `general.silence_hangover_ms` | `300` | Send time after speech stops |
| `general.jitter_frames` | `3` | Overflow mixer buffer (20 ms frames) |
| `discord.command_role_ids` | `[]` | Roles that can toggle (empty = all) |
| `fluxer.livekit_url` | local edge | `""` uses the URL from Fluxer |
| `fluxer.host_header` | none | Host name for local requests |
| `[[links]]` | none | Fixed channel pairs |

The bridge checks the config when it starts and stops with a clear message
if a required value is missing.

## Build and run

```sh
cargo build --release
./target/release/voice-pipe                 # reads ./config.toml
./target/release/voice-pipe /path/to/config.toml
```

Stop the bridge with Ctrl+C or `SIGTERM`. The bots leave voice and log out
before the process stops (10 s limit).

Log detail:

```sh
RUST_LOG=info,voice_pipe=debug ./target/release/voice-pipe
```

### Run as a systemd service

`/etc/systemd/system/voice-pipe.service`:

```ini
[Unit]
Description=voice-pipe Discord and Fluxer voice bridge
After=network-online.target docker.service
Wants=network-online.target

[Service]
WorkingDirectory=/opt/voice-pipe
ExecStart=/opt/voice-pipe/voice-pipe /opt/voice-pipe/config.toml
Restart=on-failure
RestartSec=10
User=voice-pipe

[Install]
WantedBy=multi-user.target
```

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now voice-pipe
journalctl -u voice-pipe -f
```

## Commands

Send the commands in a text channel of the Discord server. The examples use
the default prefix `vp!`.

| Command | Result |
| :--- | :--- |
| `vp!help` | Shows the list of commands. |
| `vp!toggle` | Turns the bridge off or on. When it is off, all bots leave voice and the bridge ignores users. When `command_role_ids` is set, only those roles can use it. |
| `vp!status` | Shows whether the bridge is on, the free proxies, where the main bots are, and each active link: users on each side, mirrors, and seconds of relayed speech. |

Example status:

```
voice-pipe status
Bridge: on
Discord proxies free: 1 of 2 · main bot: not in voice
Fluxer proxies free: 0 of 1 · main bot: overflow in General
Active links: 1

General · audio relay on
Discord users: 3 · Fluxer users: 1
In Discord: Bob
In Fluxer: Alice, Multiple Users (2)
Relayed speech: 41.2 s to Discord, 63.8 s to Fluxer
```

The toggle state is not saved. After a restart, `start_enabled` decides
it.

## Troubleshooting

| Problem | Cause and fix |
| :--- | :--- |
| `close code 4014` | Turn on the Message Content intent for the Discord main bot. |
| `close code 4004` | A token is not correct. Copy it again. |
| `discord login failed` or `fluxer login failed` | The token or `api_url` is wrong, or Fluxer is not running. |
| `did not send guild` | The bot is not in the server, or `guild_id` is wrong. |
| `no answer to the join` | Discord ignored the voice join. Check the Connect permission (not only View Channel) and the user limit of the channel, because bots count toward it. |
| `proxy token does not work` | That proxy token is not correct. The bridge skips it. |
| Proxy joins but has the wrong name | The bot needs the Change Nickname permission. |
| Fluxer avatar does not copy | Check `avatar_url` against the avatar URLs of the Fluxer web app. |
| Fluxer voice connects, no audio | See item 4 in [Network setup](#network-setup). |
| Background noise relays | Make `voice_threshold` higher. |
| Ends of words are cut | Make `silence_hangover_ms` higher. |
| Channels do not link | The names must be the same on both platforms, or add a `[[links]]` entry. |

## Limits

- **One overflow channel for each platform.** The main bot can be in only
  one voice channel for each server. If two links need overflow at the same
  time, the second link has no overflow voice until the first one releases
  the main bot. Add proxies to prevent this.
- **One proxy for each person.** A link with 5 Fluxer users needs 5
  Discord proxies for all of them to show by name. Users without a proxy
  share the main bot.
- **Rate limits.** Nicknames and avatars change through the API. The
  bridge copies an avatar only when a proxy gets a different person.
  Discord and Fluxer can still delay many quick changes.
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
  audio/            20 ms audio frames, mixer, and jitter buffer
  gateway/          Gateway connection (log in, resume, voice view, joins)
  voice/            Voice connections: Songbird (Discord), LiveKit (Fluxer)
  bridge/
    mod.rs          Start the bridge again after errors
    hub.rs          State that all links share: main bots, proxy pool, switch
    coordinator.rs  Start and stop links, answer commands
    matching.rs     Find linked channel pairs
    link.rs         One link task: sync mirrors and relay audio
    side.rs         Bots in one channel of a link: proxies, overflow, listener
    proxy.rs        Proxy pool and proxy sessions (copy name and avatar)
    overflow.rs     Mix overflow speakers for the main bot
    status.rs       Link state and the status report
```

Run the tests:

```sh
cargo test
```

## License

voice-pipe is released under the [MIT License](LICENSE).
Copyright (c) 2026 Grimsace.
