# Security and privacy

GamePause reads local launcher metadata and process paths. It communicates only with the
configured local LM Studio address. There is no telemetry, store authentication, or cloud API.
The app does not change game files or filesystem permissions.

Local configuration, inventory, status, recovery state, and rotated logs live under
`%LOCALAPPDATA%\GamePause`. They can reveal game/model names and filesystem paths.
Review and redact them before sharing. `GAMEPAUSE_LM_API_TOKEN` is read from the environment;
it is not written into configuration or the recovery journal.

Don't include tokens or sensitive machine information in public issues. For a security
problem, use GitHub's private vulnerability reporting when available, or contact the
maintainer privately through the repository owner's profile. Public issues are suitable
for ordinary compatibility problems with sanitized examples.

The v0.1 preview is the currently supported release line. Downloads are unsigned and
accompanied by SHA-256 checksums. CI builds executable archives from the tagged source.
