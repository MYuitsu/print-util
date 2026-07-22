# print-util

Local HTTP server nhận file PDF qua API và in **ngầm** (silent) — không mở hộp thoại chọn máy in hay kích thước trang.

## Yêu cầu

- Windows 10/11 (64-bit)
- Installer đã đóng gói sẵn các PDF engine chính (không cần tải thêm sau khi cài):
  | Engine | Cách cài | Ghi chú |
  |--------|----------|---------|
  | SumatraPDF | Đóng gói trong installer | In im lặng, ưu tiên đầu tiên |
  | `gsdll64.dll` | Đóng gói trong installer | Fallback in-process |
  | Ghostscript CLI | Đóng gói trong installer | `gswin64c.exe` và resource đi kèm |
  | Adobe Acrobat / Reader | Tuỳ chọn, nếu máy đã cài | Tự động phát hiện |
  | *(fallback)* ShellExecuteW | Có sẵn trên Windows | Có thể hiện dialog |

## Cài đặt

### Cài bằng MSIX (khuyến nghị)

Tải file `print-util-x.x.x.msix` từ [Releases](../../releases) và mở bằng Windows App Installer.

Free code signing provided by [SignPath.io](https://about.signpath.io/), certificate
by [SignPath Foundation](https://signpath.org/). See the
[Code signing policy](CODE_SIGNING_POLICY.md).

MSIX cài theo user, đăng ký tray startup task và chạy local print server trong phiên đăng nhập của user. Gỡ cài đặt tại **Settings > Apps > Installed apps**.

### Build từ source

```powershell
git clone https://github.com/<user>/print-util
cd print-util
cargo build --release
```

Binary đầu ra: `target\release\print-util.exe`

Tray companion: `target\release\print-util-tray.exe`

### Build MSIX

MSIX chạy tray companion bằng packaged startup task. Tray tự nhận diện package và
khởi động `print-util.exe --console` cho user đang đăng nhập; không cài Windows Service.

Requirements: Windows 10/11 SDK (`makeappx.exe`) and a signing certificate whose
subject matches the `Publisher` in `msix/AppxManifest.xml`.

```powershell
cargo build --release --target x86_64-pc-windows-msvc

.\scripts\build-msix.ps1 `
  -Version 0.3.0 `
  -Publisher "CN=YOUR_CERTIFICATE_SUBJECT" `
  -OutputPath .\msix\Output\print-util-0.3.0.msix
```

Sign the resulting MSIX with `signtool.exe` before installing or submitting it.

GitHub Release dùng MSIX làm artifact duy nhất. Đặt repository variables
`MSIX_PACKAGE_NAME` và `MSIX_PUBLISHER_DISPLAY_NAME` theo các giá trị trong
Partner Center trước khi tạo tag phát hành.

Ghostscript và SumatraPDF được đóng gói trong MSIX. Build sẽ báo lỗi nếu thiếu
engine hoặc resource cần thiết.

> **Lưu ý license:** Ghostscript là AGPL-3.0; cần giữ thông tin license khi phân phối.

### Tray icon VNPT (tuỳ chọn)

Nếu muốn icon khay đúng branding VNPT, đặt file `vnpt.ico` cạnh `print-util-tray.exe`
hoặc tại `%ProgramFiles%\print-util\vnpt.ico`.
Nếu không có file này, app sẽ dùng icon mặc định của Windows.

Menu tray:
- `Hỗ trợ`: mở trang hỗ trợ GitHub
- `Cấu hình`: mở file `%ProgramData%\print-util\config.json`

## Chạy server

```powershell
# Port mặc định: 17474
.\print-util.exe

# Chỉ định port
.\print-util.exe 8080

# Hoặc qua biến môi trường
$env:PORT = 17474; .\print-util.exe
```

Server chỉ lắng nghe trên `127.0.0.1` (localhost), không expose ra ngoài.

## API

### `GET /health`

Kiểm tra server đang chạy.

```
200 OK
{ "status": "ok" }
```

---

### `GET /printers`

Lấy danh sách máy in khả dụng và máy in mặc định.

**Response:**
```json
{
  "default": "HP LaserJet Pro",
  "printers": ["HP LaserJet Pro", "Microsoft Print to PDF"]
}
```

---

### `POST /print`

In file PDF với khổ giấy tự nhận diện từ `MediaBox` (A4/A5). Nếu không nhận diện được thì mặc định A4.

---

### `POST /print/a4`

In file PDF và ép khổ giấy A4.

---

### `POST /print/a5`

In file PDF và ép khổ giấy A5.

---

Các endpoint `POST /print*` dùng chung request/response dưới đây.

**Request:** `multipart/form-data`

| Field | Bắt buộc | Mô tả |
|-------|----------|-------|
| `file` | ✓ | Nội dung file PDF |
| `printer` | — | Tên máy in. Bỏ trống = dùng máy in mặc định |
| `name` | — | Tên print job hiển thị ở spooler. Bỏ trống = tự sinh `doc-<unix_timestamp>` |

**Response thành công:**
```json
{ "status": "ok" }
```

**Response lỗi:**
```json
{ "error": "mô tả lỗi" }
```

**HTTP status codes:**
| Code | Ý nghĩa |
|------|---------|
| 200 | In thành công |
| 400 | Multipart lỗi hoặc thiếu field `file` |
| 500 | Lỗi engine in, lỗi nội bộ, hoặc timeout |

**Timeout:** server timeout sau `120s` cho mỗi job in và trả:
```json
{ "error": "print timed out after 120 s" }
```

## Ví dụ

### curl

```bash
# In bằng máy in mặc định
curl -X POST http://127.0.0.1:17474/print -F "file=@document.pdf"

# Chỉ định máy in
curl -X POST http://127.0.0.1:17474/print \
  -F "file=@document.pdf" \
  -F "printer=HP LaserJet Pro"
```

### PowerShell

```powershell
$response = Invoke-RestMethod -Uri http://127.0.0.1:17474/print `
  -Method POST `
  -Form @{ file = Get-Item .\document.pdf }
$response
```

### JavaScript (fetch)

```js
const form = new FormData();
form.append('file', pdfBlob, 'document.pdf');
// form.append('printer', 'HP LaserJet Pro'); // tuỳ chọn

const res = await fetch('http://127.0.0.1:17474/print', {
  method: 'POST',
  body: form,
});
const json = await res.json();
console.log(json); // { status: 'ok' }
```

### Python (requests)

```python
import requests

with open('document.pdf', 'rb') as f:
    res = requests.post(
        'http://127.0.0.1:17474/print',
        files={'file': ('document.pdf', f, 'application/pdf')},
        data={'printer': 'HP LaserJet Pro'},  # tuỳ chọn
    )
print(res.json())
```

## Chạy như Windows Service (tuỳ chọn)

Dùng [NSSM](https://nssm.cc) để chạy background:

```powershell
nssm install print-util "C:\path\to\print-util.exe"
nssm set print-util AppEnvironmentExtra PORT=17474
nssm start print-util
```

## Xem tên máy in

```powershell
Get-Printer | Select-Object Name, Default
```

## Code signing policy

Signed MSIX packages are distributed via **Windows Package Manager (winget)** and Microsoft Store.

See [CODE_SIGNING_POLICY.md](CODE_SIGNING_POLICY.md) for signing details and
[STORE_SUBMISSION.md](STORE_SUBMISSION.md) for the Microsoft Store release checklist.

## Privacy

See the [Privacy Policy](PRIVACY.md). print-util processes print jobs locally and
does not collect telemetry or send application data to remote services.

## License

MIT
