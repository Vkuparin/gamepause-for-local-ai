# Changelog

## 0.1.1 — 2026-10-03 (preview)

- Fix a crash when the tray menu remains open through a status timer tick. Windows dispatches timer messages inside the popup menu; the UI now releases state borrows before native calls.
- Prevent nested tray clicks from opening a second menu.
- Add an automatic timer/menu regression test and a separate interactive Windows test for three popup openings, each spanning two timer callbacks.
- Upgrading preserves configuration, recovery state, and startup registration.

## 0.1.0 — 2026-10-03 (preview)

- First public release, implemented in native Rust for Windows x64.
- Steam, Epic, EA, Ubisoft Connect, Battle.net, Xbox, and custom installation discovery.
- Native process polling, executable path caching, and configurable exclusions.
- Dynamic capture and verified restoration of LLM and embedding instances with complete load settings.
- Server shutdown during gaming, exit grace period, multi-game sessions, retry backoff, durable recovery, and instance locking.
- Native tray controls, console diagnostics, startup at Windows sign-in, per-user installer, portable ZIP, checksums, and dependency licenses.
- Live Steam / Witcher 3 validation and documented resource measurements.

This preview uses LM Studio's internal control protocol. See the validation and troubleshooting guides before enabling active mode on a new setup.
