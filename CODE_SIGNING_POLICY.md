# Code signing policy

Release installers are distributed via the **Windows Package Manager (winget)**.
Packages published to [microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs)
are validated and signed by Microsoft, so Windows SmartScreen does not warn users.

## Scope

Only release artifacts built directly from this repository's source code are published.
Signing covers:

- `print-util.exe` – the main server binary
- `print-util-*-setup.exe` – the Windows installer

## Build and release process

All release builds are executed via GitHub Actions ([`.github/workflows/release.yml`](.github/workflows/release.yml)).
No local or manually-produced binaries are released.

After each GitHub Release is created, [`.github/workflows/winget.yml`](.github/workflows/winget.yml)
automatically opens a pull request to `microsoft/winget-pkgs` with the updated manifest.

## Team roles

| Role | Responsibility |
|------|---------------|
| Author / Committer | Writes and commits source code |
| Reviewer | Reviews pull requests before merge |
| Approver | Merges releases to `main` and creates release tags |

## Privacy

This application does not transmit any data over the network except in direct response to API requests made explicitly by the local user. No telemetry, analytics, or background network calls are made.

## Verifying a release

SHA-256 checksums are published alongside each installer on the [Releases](../../releases) page (`SHA256SUMS.txt`).

```powershell
# Verify downloaded installer
$expected = (Get-Content SHA256SUMS.txt).Split("  ")[0]
$actual   = (Get-FileHash print-util-*-setup.exe -Algorithm SHA256).Hash.ToLower()
if ($expected -eq $actual) { "OK" } else { "MISMATCH" }
```
