# Changelog

Alle relevanten Änderungen an diesem Projekt werden hier dokumentiert.

Das Format basiert auf [Keep a Changelog](https://keepachangelog.com/de/1.1.0/),
das Projekt folgt [Semantic Versioning](https://semver.org/lang/de/).

## [Unreleased]

## [0.1.0] - 2026-10-31

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
- Tray-Icon (StatusNotifierItem): Verbindungs- und Pausenstatus, unquittierte Notfälle, letzte Meldungen,
  Verlauf als HTML-Seite, Pause/Fortsetzen, Beenden.
- `nudge status`, `nudge pause [DAUER]`, `nudge resume` (über D-Bus) und `nudge history`
  (`--app`, `--since`, `-n`, `--json`).
- Pause: Meldungen bis Priorität 1 nur in den Verlauf, Sammel-Notification am Ende, Notfälle kommen durch.
- `nudge install-service` / `uninstall-service`: systemd-User-Unit, startet mit der Desktop-Sitzung.
- Eigene Icons (App und Tray), `.desktop`-Datei, systemd-User-Unit im Paket.
- Pakete: `.deb` für amd64 und arm64, AUR-Paket `nudge-desktop`, Tarballs. CI mit GitHub Actions.
- README (englisch) mit Lizenzhinweis, Einrichtung und Datenschutz.

[Unreleased]: https://github.com/alfredch/nudge/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/alfredch/nudge/releases/tag/v0.1.0
