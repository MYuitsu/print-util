# Code signing policy

Release installers are distributed through GitHub Releases, the Microsoft Store,
and the **Windows Package Manager (winget)**. Winget validates package manifests,
but it does not sign publisher binaries.

Free code signing provided by [SignPath.io](https://about.signpath.io/), certificate
by [SignPath Foundation](https://signpath.org/).

## Scope

Only release artifacts built directly from this repository's source code are published.
The project signing policy covers artifacts built from this repository, including:

- `print-util.exe` – the main server binary
- `print-util-tray.exe` – the tray companion
- the Inno Setup uninstaller
- `print-util-*-setup.exe` – the Windows installer

Bundled upstream executables and libraries are not signed using the project's
SignPath subscription. They retain signatures supplied by their upstream publishers,
if available.

## Build and release process

All release builds are executed via GitHub Actions ([`.github/workflows/release.yml`](.github/workflows/release.yml)).
No local or manually-produced binaries are released. Until the SignPath integration
is approved and enabled, the release workflow supports a publisher-owned certificate
through the following GitHub Actions repository secrets:

- `WINDOWS_SIGNING_CERTIFICATE_BASE64` – Base64-encoded PFX issued by a CA in the Microsoft Trusted Root Program
- `WINDOWS_SIGNING_CERTIFICATE_PASSWORD` – password protecting that PFX

The workflow timestamps signatures, validates every packaged PE, and stops before
creating a GitHub Release if any signature is missing or invalid.

After each GitHub Release is created, [`.github/workflows/winget.yml`](.github/workflows/winget.yml)
automatically opens a pull request to `microsoft/winget-pkgs` with the updated manifest.

## Team roles

| Role | Member | Responsibility |
|------|--------|---------------|
| Author / Committer | [@MYuitsu](https://github.com/MYuitsu) | Writes and commits source code |
| Reviewer | [@MYuitsu](https://github.com/MYuitsu) | Reviews external contributions before merge |
| Approver | [@MYuitsu](https://github.com/MYuitsu) | Approves signing requests and creates release tags |

All project members must use multi-factor authentication for GitHub and SignPath.

## Privacy

This program will not transfer any information to other networked systems unless
specifically requested by the user or the person installing or operating it. No
telemetry, analytics, or background network calls are made. See the
[Privacy Policy](PRIVACY.md).

## Verifying a release

SHA-256 checksums are published alongside each installer on the [Releases](../../releases) page (`SHA256SUMS.txt`).

```powershell
# Verify downloaded installer
$expected = (Get-Content SHA256SUMS.txt).Split("  ")[0]
$actual   = (Get-FileHash print-util-*-setup.exe -Algorithm SHA256).Hash.ToLower()
if ($expected -eq $actual) { "OK" } else { "MISMATCH" }
```
