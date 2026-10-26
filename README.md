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
  Discord main bot also answers the chat commands. When there are not
  enough proxies for all the people in a channel, the main bot joins voice
  with the nickname **Multiple Users** and speaks for all of them.
- **Proxy bots** (zero or more for each platform). A proxy copies the
  nickname and avatar of one person from the other platform and joins voice
  as that person. A proxy on Discord shows a Fluxer user. A proxy on Fluxer
  shows a Discord user. A proxy also shows when its person is muted or
  deafened. A group bot speaks for many people, so it never shows a mute or
  a deafen. One bot in each channel is the listener, which receives the
  audio of that channel. A deafened bot gets no audio, so the bridge
  prefers a proxy whose person is not deafened as the listener. When all
  the people on the other platform are deafened, nobody can hear the
  channel, so a deafened listener loses nothing.

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

### Linked channels

A link is a Discord voice channel and a Fluxer voice channel that the bridge
joins together. The bridge finds links in two ways:

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
   audio is received or sent in this state.**
2. **Both sides have people.** On each side, one bridge bot becomes the
   *listener*. Only the listener receives audio. The bridge gives the audio
   of each speaker to the mirror of that speaker on the other platform.
   For a group, the speakers are mixed together and the group bot plays
   the mix.
3. **A person leaves.** Their mirror leaves, and the proxy gets its idle
   nickname again (`Voice Pipe Proxy 1`, `Voice Pipe Proxy 2`, ...). When
   both channels are empty, the link stops and all of its bots leave.

### Audio only when it is necessary

- Bots do not decode or subscribe to audio until people are in both
  channels. On Discord the voice driver does not decrypt or decode. On
  Fluxer the bot does not subscribe to any audio track.
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

### Fluxer traffic stays local

By default, Fluxer runs on the same machine as the bridge (see
[Fluxer on another machine](#fluxer-on-another-machine) for other setups):

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

### Fluxer on another machine

The bridge can connect to a Fluxer server on another machine, for example
a server on your LAN or a public Fluxer server. Change these settings in
the `[fluxer]` part of `config.toml`:

| Setting | Value for a public server at `chat.example.com` |
| :--- | :--- |
| `api_url` | `https://chat.example.com/api/v1` |
| `gateway_url` | `wss://chat.example.com/gateway?v=1&encoding=json` |
| `livekit_url` | `""` (use the URL that Fluxer sends) |
| `avatar_url` | The avatar URL that the Fluxer web app uses |
| `host_header` | `""` (not necessary when the URL has the host name) |

Use the same host names and paths that the Fluxer web app uses. To find
them, open the network tab of the browser developer tools in the web app.

- **Use `https://` and `wss://`.** The bot tokens go in each request. With
  `http://` or `ws://` to another machine, anybody on the network path can
  read them. The bridge writes a warning at start for each URL like this.
  A plain URL is acceptable only on a network that you trust.
- **Certificates.** The bridge accepts only certificates from a public
  certificate authority (for example Let's Encrypt or Cloudflare). A
  self-signed certificate does not work.
- **Voice media.** The bridge connects to LiveKit UDP 7882 (or TCP 7881)
  on the Fluxer server, so these ports must be open from the bridge
  machine. The hairpin NAT item above does not apply.
- **Latency.** Each extra network hop adds delay to the relayed audio.

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
| `general.start_enabled` | `true` | Start with the bridge on (when no state is saved) |
| `general.state_file` | `voice-pipe-state.toml` | File that keeps the toggle state (`""` = do not save) |
| `general.match_by_name` | `true` | Link channels with the same name |
| `general.ignore_afk_channels` | `true` | Do not link AFK channels by name |
| `general.overflow_name` | `Multiple Users` | Nickname of a group bot |
| `general.proxy_idle_name` | `Voice Pipe Proxy {number}` | Nickname of a free proxy |
| `general.voice_threshold` | `50` | Level that starts speech, in 16-bit RMS amplitude (50 is about -56 dBFS); `0` sends all audio |
| `general.silence_hangover_ms` | `300` | Send time after speech stops, in milliseconds (minimum 20) |
| `general.jitter_frames` | `3` | Overflow mixer buffer (20 ms frames) |
| `general.audio_reports` | `false` | Log a receive report (packets, loss, levels, stereo) for each Discord speaker every 10 s |
| `discord.command_role_ids` | `[]` | Discord roles that can use toggle, link, confirm, and unlink |
| `fluxer.command_role_ids` | `[]` | Fluxer roles that can use the same commands (both lists empty = everyone, with a warning at start) |
| `fluxer.livekit_url` | local edge | `""` uses the URL from Fluxer |
| `fluxer.host_header` | none | Host name for local requests |
| `[[links]]` | none | Fixed channel pairs |

The bridge checks the config when it starts and stops with a clear message
if a required value is missing.

### Units of the audio settings

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

## Build and run

```sh
cargo build --release
./target/release/voice-pipe                 # reads ./config.toml
./target/release/voice-pipe /path/to/config.toml
```

The build uses a patched copy of Songbird 0.6.1 in `vendor/songbird`.
Songbird 0.6.1 gives the DAVE encryption trailer to the Opus decoder with
the audio, which damages the audio of Discord users whose client sends
CELT (for example, some clients with a webcam on). The patch is in
`vendor/songbird-dave-trailer.patch`. When a fixed Songbird is released,
remove the `[patch.crates-io]` entry in `Cargo.toml` and the `vendor`
folder.

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
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
```

`LimitNOFILE` is necessary. Each Fluxer bot holds two WebRTC connections
with many sockets, and each Discord bot holds its own connections. The
systemd default of 1024 open files is too low for a few bots, and joins
then fail with "Too many open files".

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now voice-pipe
journalctl -u voice-pipe -f
```

## Commands

Send the commands in a text channel of the Discord server or the Fluxer
server. Send `link`, `confirm`, and `unlink` in the text chat of a voice
channel: the bridge uses that voice channel. The examples use the default
prefix `vp!`.

| Command | Result |
| :--- | :--- |
| `vp!help` | Shows the list of commands. |
| `vp!toggle` | Turns the bridge off or on. When it is off, all bots leave voice and the bridge ignores users. Needs a command role. |
| `vp!status` | Shows whether the bridge is on, the free proxies, where the main bots are, and each active link: users on each side, mirrors, and seconds of relayed speech. |
| `vp!link` | Starts to link this voice channel. The bot answers "Listening to `vp!confirm`". The request ends after 5 minutes, or when someone sends a new `vp!link`. Needs a command role. |
| `vp!confirm` | In a voice channel on the other platform: links this channel with the channel of the request. The link starts at once and is saved as a `[[links]]` entry in `config.toml`. Needs a command role. |
| `vp!unlink` | Removes the fixed link of this voice channel, from the bridge and from `config.toml`. Needs a command role. |

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
a valid config before it writes it, and keeps the file permissions. The bot
user must be able to write to `config.toml` and to its directory. A channel
that is already in a fixed link cannot be linked again: send `vp!unlink`
first. `unlink` cannot remove a link by name; rename a channel or set
`match_by_name = false` for that.

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

The toggle state is saved in `state_file` (default
`voice-pipe-state.toml` in the working directory), so it stays the same
after a restart. `start_enabled` is used only when there is no saved state.
The bot user must be able to write to that directory (for the systemd
example below, `/opt/voice-pipe`).

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
| Background noise relays | Ask the person to use push-to-talk or the noise suppression of their app. |
| Ends of words are cut | Make `silence_hangover_ms` higher. |
| A mirror always shows as speaking | Make `voice_threshold` higher. |
| Quiet speakers are not relayed | Make `voice_threshold` lower. |
| Channels do not link | The names must be the same on both platforms, or add a `[[links]]` entry. |

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
    side.rs         Bots in one channel of a link: proxies, group bot, listener
    proxy.rs        Proxy pool and proxy sessions (copy name and avatar)
    overflow.rs     Mix the speakers of a group for the group bot
    status.rs       Link state and the status report
vendor/
  songbird/         Songbird 0.6.1 with the DAVE trailer fix (ISC License)
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
