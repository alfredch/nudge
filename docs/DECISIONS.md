# Entscheidungen

Ergänzt `PLAN.md`. Bei Widerspruch gilt dieses Dokument.

## Abgleich mit der API-Doku (Stand 2026-10-30, https://pushover.net/api/client)

- **User-Agent ist Pflicht** bei jedem HTTP-Request: `nudge/<version> (Linux; unofficial Pushover Open Client)`.
- Login liefert `id` (User-Key) und `secret`. Nur `secret` wird gespeichert.
- Login mit 2FA: HTTP 412 → `TwoFactorRequired`, erneut mit `twofa` senden.
- Gerät registrieren: `os=O`, Name `[A-Za-z0-9_-]{1,25}`. Fehler kommen als `status: 0` mit `errors`-Objekt.
- Löschen: `POST /1/devices/{device_id}/update_highest_message.json` mit `secret` und `message=<höchste id>`.
- Quittieren nur bei aktiver Nutzeraktion, nie bei Timeout oder Schließen der Notification.
- WebSocket-Frames: `#` Keepalive, `!` neue Meldungen, `R` Reconnect, `E` permanenter Fehler (kein Auto-Reconnect), `A` anderswo eingeloggt (kein Auto-Reconnect).
- Nach einem Verbindungsabbruch nach erfolgreichem Login: vor dem Reconnect warten. Fehlgeschlagene HTTP-Requests nie ohne Delay wiederholen.
- Icons: `https://api.pushover.net/icons/<icon>.png`, Sounds: `https://api.pushover.net/sounds/<sound>.mp3`. Beides muss gecacht werden.
- Pflichthinweis in App (`--version`, `--help`, Tray-Tooltip) und README: „unofficial Pushover Open Client“, nicht von Pushover veröffentlicht oder unterstützt. Das Pushover-Logo ist nicht das Haupt-Icon.

## Produktentscheidungen

| Thema | Entscheidung |
|---|---|
| App-ID / D-Bus | `io.github.alfredch.Nudge`, Objektpfad `/io/github/alfredch/Nudge` |
| Start-Backlog | Alles abholen, in den Verlauf schreiben und löschen, dazu eine Sammel-Notification „N Meldungen während Abwesenheit“. Unquittierte Emergency-Meldungen (`priority >= 2`, `acked != 1`) werden einzeln angezeigt. |
| Emergency | Critical-Notification, `resident`, ohne Timeout, Action „Quittieren“. Bei Server-Wiederholungen erneut zeigen (gleiche Notification ersetzen, über `receipt`). Tray bleibt markiert bis zur Quittierung. Kein eigenes Fenster. |
| Credentials | `oo7` (Secret Service über zbus), Fallback auf Datei mit `0600` unter `~/.local/state/nudge/` mit Warnung |
| Gerätename | Default: Hostname, bereinigt und gekürzt. Beim Login änderbar. |
| Toolchain | `rust-toolchain.toml` mit fester Version 1.99.1, Edition 2024 |
| Releases | glibc, x86_64 und aarch64, `.deb` (cargo-deb) und AUR (PKGBUILD) |
| Git | Haupt-Branch `main` |

## Offene Punkte

- Flatpak: später.
- Ob Live-Meldungen eine eigene Ruhezeiten-Ausnahme pro App bekommen sollen: in M4 entscheiden.
