# Microsoft Store submission

The release workflow produces a signed MSIX package for Microsoft Store, GitHub Releases, and winget.

## Blocking prerequisites

1. Enroll in Microsoft Partner Center and reserve the product name.
2. Obtain a code-signing certificate issued by a CA in the Microsoft Trusted Root Program, or complete the approved SignPath Foundation integration. A self-signed certificate is only suitable for local MSIX testing.
3. Set the GitHub repository variables `MSIX_PACKAGE_NAME` and `MSIX_PUBLISHER_DISPLAY_NAME` to the exact values assigned in Partner Center.
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

The release workflow signs the application binaries, bundled PE files, and the final MSIX package. It then verifies the package and payload signatures before publishing release artifacts.

## Create a release

Ensure the tag version matches `package.version` in `Cargo.toml` before tagging.

Create and push the release tag:

```powershell
git tag v0.3.0
git push origin v0.3.0
```

After the Release workflow succeeds, do not replace the uploaded installer. Microsoft requires the binary at a submitted versioned URL to remain unchanged.

Verify the published file locally:

```powershell
.\scripts\test-store-readiness.ps1 `
    -MsixPath .\msix\Output\print-util-0.3.0.msix
```

## Partner Center MSIX package fields

Use the following values for version `0.3.0`:

| Field | Value |
|---|---|
| Package type | MSIX |
| Architecture | x64 |
| Package URL | Use the direct URL of `print-util-0.3.0.msix`; GitHub Release redirect URLs are not accepted by Partner Center |
| Installer parameters | Not applicable for MSIX |
| Installer behavior | Package installation is managed by Windows; the tray startup task launches the local server for the signed-in user |
| Minimum OS | Windows 10 version 1809 x64 |
| Support URL | `https://github.com/MYuitsu/print-util/issues` |
| Privacy policy URL | `https://github.com/MYuitsu/print-util/blob/main/PRIVACY.md` |
| License URL | `https://github.com/MYuitsu/print-util/blob/main/LICENSE` |

For every later version, submit a new versioned URL. Do not use a `latest` download URL.

## Certification notes

Provide these notes to the certification team:

- The MSIX package does not install a Windows Service; it uses a packaged startup task and runs the local server in the signed-in user's context.
- The local server listens only on `127.0.0.1:17474`; it is not reachable from other computers.
- The local API accepts PDFs from software running on the same computer and forwards print jobs to the Windows print spooler.
- The package contains all runtime printing components and does not download payloads during installation.
- The application has no analytics, advertising, telemetry, or background calls to publisher servers.
- The MSIX package starts the tray process at the next interactive user sign-in. The tray process starts the local server with `--console` in the packaged user's context.

No test account is required because the application has no authentication.

## Final checks

Before selecting **Submit for certification**:

- Install the MSIX on a clean Windows 10 or Windows 11 x64 VM.
- Confirm Windows App Installer completes without an error.
- Confirm the packaged startup task launches `print-util-tray.exe` after user sign-in.
- Confirm `GET http://127.0.0.1:17474/health` returns HTTP 200.
- Submit a PDF to `/print` and verify that Windows receives the print job.
- Uninstall from Windows Settings and confirm the packaged processes and installed files are removed.
- Confirm every packaged `.exe` and `.dll` has a valid Authenticode signature.
- Upload listing artwork and screenshots that accurately show the tray menu and the installed utility.
