# PLAN: Nativer Linux-Client für Pushover (Rust)

Arbeitstitel: `nudge` (Name frei änderbar, darf aber **nicht** „Pushover“ als Produktnamen tragen).
Inoffizieller Open-Source-Client, der Pushover-Meldungen über die **Open Client API** empfängt und als native Linux-Desktop-Notifications anzeigt. Nur Linux, Wayland und X11.

Referenz für alle API-Details: https://pushover.net/api/client. Prüfe Endpunkte, Parameter und Frame-Codes dort gegen, bevor du implementierst. Die Angaben unten sind der erwartete Stand.

---

## 1. Ziele

- Läuft als Hintergrunddienst (systemd user service), unabhängig von einem Browser.
- Echtzeit-Empfang über WebSocket, robust bei Netzwechsel, Suspend/Resume und Serverneustarts.
- Native Notifications über freedesktop D-Bus, mit Actions (z. B. „Quittieren“, „Link öffnen“).
- Tray-Icon (StatusNotifierItem), Verlauf, Sounds, sichere Ablage der Credentials.
- Saubere CLI für Setup und Diagnose.
- Veröffentlichung als Open Source mit CI, Releases und Paketen.

Nicht-Ziele: Windows/macOS, Versand von Meldungen als Kernfunktion (nur `test`-Hilfsbefehl), GUI-Framework für komplexe Fenster.

---

## 2. Architektur

Cargo-Workspace:

```
nudge/
├── Cargo.toml              # workspace
├── rust-toolchain.toml     # stable, fixierte Version
├── crates/
│   ├── nudge-api/          # reiner API-Client, kein UI, kein D-Bus
│   └── nudge/              # Binary: CLI, Daemon, Notifications, Tray, Storage
├── packaging/              # systemd unit, .desktop, Icons, PKGBUILD
└── .github/workflows/
```

**`nudge-api`** (Library, async, `tokio` + `reqwest` mit `rustls`, `tokio-tungstenite`, `serde`, `thiserror`):
- `login(email, password, twofa: Option<&str>) -> Secret`. HTTP 412 bedeutet, dass 2FA benötigt wird, dann eigener Fehlertyp `TwoFactorRequired`.
- `register_device(secret, name) -> DeviceId` (`os=O`; Namensregeln laut Doku validieren: max. 25 Zeichen, `[A-Za-z0-9_-]`).
- `fetch_messages(secret, device_id) -> Vec<Message>`
- `delete_up_to(secret, device_id, highest_id)` (`/1/devices/{device_id}/update_highest_message.json`)
- `acknowledge(secret, receipt)` (`/1/receipts/{receipt}/acknowledge.json`)
- `PushStream`: WebSocket zu `wss://client.pushover.net/push`, sendet `login:{device_id}:{secret}\n` und liefert ein Enum:
  - `#` → `Keepalive`
  - `!` → `NewMessages` (dann per HTTP abholen)
  - `R` → `Reconnect`
  - `E` → `Error` (Re-Login nötig, **nicht** automatisch reconnecten)
  - `A` → `LoggedInElsewhere` (Session beenden, Nutzer informieren)
- Typisierte `Message` mit allen Feldern (`id`, `umid`, `title`, `message`, `app`, `aid`, `icon`, `date`, `priority`, `sound`, `url`, `url_title`, `receipt`, `acked`, `html`, …). Optionale Felder als `Option<T>`.
- Keine Secrets in `Debug`-Ausgaben (eigener `Secret`-Newtype mit maskiertem `Debug`).

**`nudge`** (Binary):
- `clap`-CLI, `tracing` + `tracing-subscriber` (journald-freundlich), `anyhow` auf Binary-Ebene.
- Daemon-Kern als Task-Struktur: WebSocket-Supervisor → Fetch → Persist → Notify → Delete. Kommunikation über `tokio::sync::mpsc`.

---

## 3. Features („bells and whistles“)

**Verbindung & Robustheit**
- Exponentielles Backoff mit Jitter, Watchdog: kein Keepalive innerhalb von ~90 s führt zum Reconnect.
- Erkennung von Suspend/Resume und Netzwerkwechsel (logind `PrepareForSleep` und NetworkManager über `zbus`, best effort) mit sofortigem Reconnect und Fetch.
- Beim Start immer ausstehende Meldungen abholen, bevor der WebSocket läuft.
- Löschen erst **nach** erfolgreichem Persistieren (keine verlorenen Meldungen).

**Notifications** (`notify-rust`, D-Bus/zbus-Backend)
- Mapping der Priorität: `-2` → nur Verlauf, keine Anzeige; `-1` → leise, Urgency low; `0` → normal; `1` → Urgency critical, ignoriert Ruhezeiten; `2` (Emergency) → critical, persistent, Action „Quittieren“ ruft `acknowledge` auf, bis zur Quittierung im Tray markiert.
- Actions: „Link öffnen“ (wenn `url` gesetzt, via `xdg-open`/`open`-Crate), „Kopieren“.
- App-Icons als Notification-Icon (aus Cache).
- HTML-Meldungen (`html=1`): auf das von freedesktop unterstützte Markup reduzieren bzw. sicher sanitizen.
- Ruhezeiten (konfigurierbar) und „Pause für X Minuten“ über Tray/CLI.

**Sounds** (`rodio`)
- Pushover-Sounds abspielen, Mapping über das `sound`-Feld, Fallback auf Default-Sound, global abschaltbar, Lautstärke konfigurierbar.

**Cache** (XDG via `directories`)
- Icons und Sounds unter `~/.cache/nudge/` ablegen, nur einmal herunterladen (Pflicht laut API-Doku). Neuer Icon-Name = neuer Download.

**Credentials**
- `secret` und `device_id` im Secret Service (`keyring`-Crate mit Linux-Secret-Service-Backend bzw. `oo7`). Passwort wird **nie** gespeichert.
- Fallback mit Warnung: Datei mit `0600` in `~/.local/state/nudge/`, nur wenn kein Secret Service verfügbar ist.

**Verlauf** (`rusqlite`, bundled)
- Alle Meldungen in SQLite unter `~/.local/share/nudge/history.db`, mit konfigurierbarer Aufbewahrungsdauer.
- CLI: `nudge history [--app X] [--since …] [--json]`.

**Tray** (`ksni`, StatusNotifierItem, kein GTK nötig)
- Icon-Zustand: verbunden / getrennt / ungelesene Emergency.
- Menü: letzte 10 Meldungen, Pause/Fortsetzen, Ruhezeiten an/aus, „Verlauf öffnen“ (Terminal oder `$EDITOR` mit JSON-Export ist ausreichend), Beenden.
- Hinweis im README: GNOME braucht die AppIndicator-Extension.

**Konfiguration**
- TOML unter `~/.config/nudge/config.toml` (`serde` + `toml`), mit dokumentierten Defaults, Reload per `SIGHUP`.

**CLI**
- `nudge login` (interaktiv, Passwort via `rpassword`, 2FA-Abfrage, Geräteregistrierung)
- `nudge logout` (lokale Credentials löschen)
- `nudge daemon` (Vordergrund, für systemd)
- `nudge status` (Verbindung, Gerät, letzte Meldung)
- `nudge history`
- `nudge test` (optional: Testmeldung mit App-Token aus der Config senden)
- `nudge install-service` (schreibt und aktiviert die systemd-User-Unit)

**Single Instance**
- Lockfile oder D-Bus-Name `org.nudge.Daemon`, damit nicht zwei Daemons parallel laufen. Die CLI spricht mit dem laufenden Daemon über D-Bus (`status`, `pause`).

---

## 4. Qualität & Tests

- `cargo fmt`, `cargo clippy -- -D warnings`, `cargo deny` (Lizenzen, Advisories).
- Unit-Tests: Frame-Parsing, Message-Deserialisierung (Fixtures aus der Doku), Prioritäts-Mapping, Ruhezeiten-Logik.
- Integrationstests für `nudge-api` mit `wiremock` (HTTP) und einem lokalen Test-WebSocket-Server (Reconnect, `E`, `A`, ausbleibende Keepalives).
- End-to-End manuell: Meldungen über die bestehenden Python-Sender aus anderen Projekten auslösen, inkl. Priorität 2 mit Quittierung.

---

## 5. Packaging & Veröffentlichung

- Lizenz: MIT OR Apache-2.0.
- `packaging/nudge.service` (systemd user unit, `Restart=on-failure`), `.desktop`-Datei, Icons (SVG).
- `cargo-deb` für `.deb`, `PKGBUILD` für das AUR. Flatpak erst später (Secret Service/Tray im Sandbox-Kontext prüfen).
- GitHub Actions: CI (fmt, clippy, test, deny) und Release via `cargo-dist` (x86_64 und aarch64, musl- oder glibc-Build festlegen).
- README: inoffizieller Client, nicht von Superblock unterstützt; jeder Nutzer benötigt eine **Pushover-for-Desktop-Lizenz** (Open Client zählt als Desktop-Gerät, 30 Tage Testzeit); Setup, GNOME-Tray-Hinweis, Datenschutz (wo was gespeichert wird).
- `CHANGELOG.md` (Keep a Changelog), SemVer.

---

## 6. Meilensteine

1. **M1 – API-Kern:** `nudge-api` komplett inkl. Tests gegen Mocks.
2. **M2 – Minimaler Daemon:** `login`, `daemon`, Notification-Anzeige, Löschen nach Empfang, Credentials im Keyring.
3. **M3 – Robustheit:** Backoff, Watchdog, Suspend/Netzwerk-Erkennung, Single Instance.
4. **M4 – Komfort:** Prioritäten-Mapping, Emergency-Quittierung, Sounds, Cache, Verlauf, Ruhezeiten.
5. **M5 – Tray & CLI-Ausbau:** `ksni`-Tray, `status`/`history`/`pause` über D-Bus, `install-service`.
6. **M6 – Release:** Packaging, CI/CD, README, erste Version `0.1.0`.

Nach jedem Meilenstein: lauffähiger Stand, Tests grün, kurzer Eintrag im CHANGELOG.

---

## 7. Hinweise für die Umsetzung

- IDE ist RustRover: keine IDE-spezifischen Dateien einchecken, `.idea/` in `.gitignore`.
- Kein `unwrap()` außerhalb von Tests, keine Panics im Daemon-Loop.
- API-Höflichkeit: kein Polling als Ersatz für den WebSocket, Fetch nur bei `!` oder (Re-)Connect.
- Bei Unklarheiten in der API-Doku nachfragen statt raten.
