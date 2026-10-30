# Changelog

Alle relevanten Änderungen an diesem Projekt werden hier dokumentiert.

Das Format basiert auf [Keep a Changelog](https://keepachangelog.com/de/1.1.0/),
das Projekt folgt [Semantic Versioning](https://semver.org/lang/de/).

## [Unreleased]

### Added
- Cargo-Workspace mit `nudge-api` (Library) und `nudge` (Binary).
- `nudge-api`: HTTP-Client für Login (inkl. 2FA), Geräteregistrierung, Abruf,
  Löschen und Quittieren von Meldungen sowie Icon- und Sound-Download, jeweils mit
  dem vorgeschriebenen User-Agent.
- `nudge-api`: `PushStream` für die WebSocket-Verbindung mit typisierten Frames.
- `nudge-api`: `Secret` mit maskierter Debug-Ausgabe und Nullung beim Drop.
- Tests mit wiremock und einem lokalen WebSocket-Testserver.
- `nudge login [--new-device]` und `nudge logout`: interaktiver Login mit 2FA, Geräteregistrierung,
  Credentials im Secret Service (Fallback: Datei mit 0600).
- `nudge daemon`: Echtzeit-Empfang, Verlauf in SQLite, Notifications mit Prioritäten-Mapping,
  Sammel-Notification für Meldungen aus der Offline-Zeit, Löschen erst nach dem Speichern.
- Exponentielles Backoff mit Jitter beim Neuverbinden (5 s bis 5 min).
- Sofortiges Neuverbinden nach Resume aus dem Standby und bei Netzwerkwechsel (logind, NetworkManager).
- Nur ein Daemon gleichzeitig (D-Bus-Name `io.github.alfredch.Nudge`).
- HTML-Meldungen behalten Fett, Kursiv und Unterstrichen. Farben entfallen, Links werden als „Text (URL)“ angezeigt.
- Buttons an Notifications: „Acknowledge“ für Notfallmeldungen (Priorität 2), „Open link“ und „Copy“
  (mit `wl-copy` oder `xclip`). Wiederholungen einer Notfallmeldung ersetzen die vorherige Notification.
- Sounds der Meldungen (rodio), Lautstärke und Default-Sound konfigurierbar.
- Icons der absendenden Apps. Icons und Sounds werden einmalig nach `~/.cache/nudge/` geladen.
- Konfigurationsdatei `~/.config/nudge/config.toml`, neu laden mit `SIGHUP`. Beispiel: `packaging/config.toml`.
- Ruhezeiten: Priorität -1 und 0 kommen still an.
- Aufbewahrungsdauer für den Verlauf (Default 365 Tage).
