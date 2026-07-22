# Microsoft Store submission

This project is submitted as an unpackaged Win32 application using its standalone Inno Setup EXE installer.

## Blocking prerequisites

1. Enroll in Microsoft Partner Center and reserve the product name.
2. Obtain an Authenticode code-signing certificate issued by a CA in the Microsoft Trusted Root Program. A self-signed certificate is not accepted.
3. Replace `AppPublisher` in `installer/setup.iss` with the legal publisher name used for the Store account and signing certificate.
4. Prepare Store listing artwork and screenshots in Partner Center.

## Configure release signing

Export the code-signing certificate and private key as a password-protected PFX. Convert it to Base64 without line breaks:

```powershell
[Convert]::ToBase64String([IO.File]::ReadAllBytes('.\store-signing.pfx')) |
    Set-Clipboard
```

Create these GitHub Actions repository secrets:

| Secret | Value |
|---|---|
| `WINDOWS_SIGNING_CERTIFICATE_BASE64` | Base64 PFX content |
| `WINDOWS_SIGNING_CERTIFICATE_PASSWORD` | PFX password |

The release workflow signs the application binaries, bundled unsigned Ghostscript PE files, the Inno Setup uninstaller, and the final installer. It then runs `scripts/test-store-readiness.ps1` and refuses to publish files with missing or invalid signatures.

## Create a release

Ensure these versions are identical before tagging:

- `Cargo.toml`: `package.version`
- `installer/setup.iss`: `AppVersion`

Create and push the release tag:

```powershell
git tag v0.3.0
git push origin v0.3.0
```

After the Release workflow succeeds, do not replace the uploaded installer. Microsoft requires the binary at a submitted versioned URL to remain unchanged.

Verify the published file locally:

```powershell
.\scripts\test-store-readiness.ps1 `
    -InstallerPath .\installer\Output\print-util-0.3.0-setup.exe `
    -RequireInstaller
```

## Partner Center package fields

Use the following values for version `0.3.0`:

| Field | Value |
|---|---|
| Package type | EXE installer |
| Architecture | x64 |
| Installer URL | `https://github.com/MYuitsu/print-util/releases/download/v0.3.0/print-util-0.3.0-setup.exe` |
| Silent install parameters | `/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP-` |
| Installer behavior | Standalone/offline installer; requires elevation |
| Minimum OS | Windows 10 x64 |
| Support URL | `https://github.com/MYuitsu/print-util/issues` |
| Privacy policy URL | `https://github.com/MYuitsu/print-util/blob/main/PRIVACY.md` |
| License URL | `https://github.com/MYuitsu/print-util/blob/main/LICENSE` |

For every later version, submit a new versioned URL. Do not use a `latest` download URL.

## Certification notes

Provide these notes to the certification team:

- The installer requests administrator elevation to install and start the `print-util` Windows Service.
- The service listens only on `127.0.0.1:17474`; it is not reachable from other computers.
- The local API accepts PDFs from software running on the same computer and forwards print jobs to the Windows print spooler.
- The installer contains all runtime printing components and does not download payloads during installation.
- The application has no analytics, advertising, telemetry, or background calls to publisher servers.
- Silent installation intentionally does not launch the tray process. The tray shortcut starts at the next interactive user sign-in.

No test account is required because the application has no authentication.

## Final checks

Before selecting **Submit for certification**:

- Install silently on a clean Windows 10 or Windows 11 x64 VM.
- Confirm the installer exits with code `0` and shows no UI except UAC.
- Confirm the `print-util` service reaches the Running state.
- Confirm `GET http://127.0.0.1:17474/health` returns HTTP 200.
- Submit a PDF to `/print` and verify that Windows receives the print job.
- Uninstall silently and confirm the service and installed files are removed.
- Confirm every packaged `.exe` and `.dll` has a valid Authenticode signature.
- Upload listing artwork and screenshots that accurately show the tray menu and the installed utility.
