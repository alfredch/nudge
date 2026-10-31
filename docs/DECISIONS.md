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
| Ruhezeiten | Priorität -1 und 0 werden still zugestellt: Urgency low, kein Ton. Priorität 1 und 2 sind nicht betroffen. Keine Ausnahmen pro App (wer durchkommen will, sendet mit Priorität 1). |
| Kopieren | Über `wl-copy` (Wayland) oder `xclip` (X11/Xwayland). Der Button erscheint nur, wenn eines davon installiert ist. Paket: `Recommends: wl-clipboard`. |
| Notifications | Direkt über zbus statt notify-rust: eine Verbindung für `Notify` und die Action-Signale, keine `unwrap()` in Fremdcode. |
| Sounds | rodio, das Audiogerät wird pro Sound geöffnet. Ohne `sound`-Feld wird `sound.default` gespielt (Default `pushover`, wie in der Message-API dokumentiert). Still sind `no` und `none` sowie `vibrate`. Verifiziert am 2026-10-31: Meldungen tragen zweistellige Codes, nicht die Namen aus der Message-API. „None (silent)“ kommt als `no` an, „Bike“ als `bk` (`sounds/bk.mp3` existiert). Der Code für „Vibrate Only“ ist noch offen. Laufzeitabhängigkeit: libasound2. |

## Offene Punkte

- Flatpak: später.
- Wie Emergency-Wiederholungen beim Client ankommen (neue `id` mit gleichem `receipt`?), steht nicht in der Doku. nudge ersetzt die Notification über den Receipt. Beim M4-Test prüfen und hier festhalten.

## Verifiziert am echten Server (2026-10-30, `cargo test -p nudge-api --test live -- --ignored`)

- TLS und WebSocket zu `wss://client.pushover.net/push` funktionieren mit rustls/aws-lc-rs.
- Ungültiges Gerät oder Secret am WebSocket: Frame `E`, danach schließt der Server.
- Ungültiges Secret bei `GET /1/messages.json`: HTTP 403, `errors: ["secret is invalid"]`. Ein 4xx bei bestehender Session heißt also: Re-Login nötig.
