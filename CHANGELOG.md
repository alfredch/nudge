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
