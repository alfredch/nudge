# Nudge

**Nudge is an unofficial Pushover Open Client for the Linux desktop.** It receives
your [Pushover](https://pushover.net) messages in real time and shows them as
native desktop notifications, with sounds, a tray icon and a local history.

> Nudge is not released, endorsed or supported by Pushover or Superblock, LLC.
> "Pushover" is a trademark of Superblock, LLC.

## Before you start

- **You need a Pushover for Desktop license.** Nudge registers as a desktop
  device on your account and counts as one. Pushover's terms say each user
  "must have or purchase a Pushover for Desktop license within 30 days of
  activation of an Open Client device". The license is per user and covers all
  of that user's desktop devices.
- Nudge is meant for **one user's own messages**. Pushover does not allow
  using an Open Client to deliver notifications to multiple users of one account.
- Linux only, on Wayland or X11, with a desktop that has a notification server
  (GNOME, KDE Plasma, Xfce, …).

## Features

- Real-time delivery over Pushover's WebSocket API. Messages that arrive
  while the computer is off show up as one summary notification.
- Priorities: -2 goes to the history only, -1 arrives silently, 1 is shown as
  urgent, and emergencies (2) stay on screen with an **Acknowledge** button
  until you acknowledge them.
- **Open link** and **Copy** buttons. Copy needs `wl-copy` (Wayland) or `xclip`.
- Message sounds and app icons, downloaded once and cached.
- HTML messages keep bold, italic, underline and links.
- Quiet hours, and a pause for 15 minutes to "until resumed". Emergencies
  always get through.
- Tray icon with recent messages, pause/resume and the history as a web page.
- Reconnects on its own after a network change or suspend.
- Credentials are kept in the Secret Service (GNOME Keyring, KWallet), and
  your password is never stored.

## Install

### Debian / Ubuntu

Download the `.deb` for your architecture (`amd64` or `arm64`) from the
[releases](https://github.com/alfredch/nudge/releases) and install it:

```sh
sudo apt install ./nudge_*.deb
```

### Arch Linux

Install the `nudge` package from the AUR, for example with `paru -S nudge`.

### From source

You need Rust (see `rust-toolchain.toml`) and the ALSA headers
(`libasound2-dev` on Debian/Ubuntu, `alsa-lib` on Arch):

```sh
cargo install --locked --git https://github.com/alfredch/nudge nudge
```

## Set up

```sh
nudge login             # e-mail, password, 2FA code if enabled; registers this computer
nudge install-service   # start nudge with your desktop session, and now
```

`nudge login` asks for a device name, which defaults to the hostname. Change
it if another device on your account already uses that name.

Send a test message from [pushover.net](https://pushover.net) and it should
appear within a second.

**After upgrading** the package, restart the service: `systemctl --user restart nudge`.

## Usage

```sh
nudge status                 # connection, pause, last message
nudge pause 30m              # or 2h, 1d; without a duration: until resumed
nudge resume
nudge history -n 20          # also --app NAME, --since 2d or --since 2026-10-30, --json
journalctl --user -u nudge -f   # log
nudge logout                 # remove the stored credentials
nudge uninstall-service
```

## Configuration

Nudge works without any configuration. To change something, create
`~/.config/nudge/config.toml`. The documented example with all defaults is
[`packaging/config.toml`](packaging/config.toml), and the packages install it as
`/usr/share/doc/nudge/config.toml.example`.

```toml
[sound]
volume = 70

[quiet_hours]
enabled = true
start = "22:00"
end = "07:00"
```

Reload after editing: `systemctl --user kill --signal=HUP nudge`.

## Desktop notes

- **GNOME** has no tray by default. Install the *AppIndicator and
  KStatusNotifierItem Support* extension (`gnome-shell-extension-appindicator`
  on Debian/Ubuntu, where it is usually preinstalled). Without it Nudge works
  normally but shows no tray icon.
- Notification servers only support bold, italic and underline, so **colors**
  in HTML messages are not shown.
- Sounds play through ALSA, which PipeWire and PulseAudio handle on current
  desktops.

## Privacy

Nudge talks only to Pushover's servers (`api.pushover.net`,
`client.pushover.net`), and only over TLS. It stores:

| What | Where |
|---|---|
| Device id and session secret | Secret Service (keyring). Without one: `~/.local/state/nudge/credentials.json`, mode 0600 |
| Message history | `~/.local/share/nudge/history.db` (SQLite). Kept for 365 days by default (`history.keep_days`) |
| Icons, sounds, history page | `~/.cache/nudge/` |
| Configuration | `~/.config/nudge/config.toml` |

Messages are deleted from Pushover's servers once Nudge has stored them, as
the Open Client API requires. The log contains message ids, apps and titles,
never the message text.

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or
[MIT](LICENSE-MIT), at your option.
