# print-util

Local HTTP server nhận file PDF qua API và in **ngầm** (silent) — không mở hộp thoại chọn máy in hay kích thước trang.

## Yêu cầu

- Windows 10/11 (64-bit)
- Một trong các PDF engine sau (theo thứ tự ưu tiên):
  | Engine | Cách cài | Ghi chú |
  |--------|----------|---------|
  | `gsdll64.dll` | Copy vào cùng thư mục exe | **Khuyến nghị** — in-process, nhanh nhất |
  | SumatraPDF | [sumatrapdfreader.org](https://www.sumatrapdfreader.org) | Per-user hoặc per-machine |
  | Ghostscript CLI | [ghostscript.com](https://ghostscript.com/releases/gsdnld.html) | `gswin64c` trên PATH |
  | Adobe Acrobat / Reader | — | Tự động phát hiện nếu đã cài |
  | *(fallback)* ShellExecuteW | — | Có thể hiện dialog nếu handler mặc định là Chrome/Edge |

## Cài đặt

### Dùng installer (khuyến nghị)

Tải file `print-util-x.x.x-setup.exe` từ [Releases](../../releases) và chạy.

Installer sẽ:
- Cài binary vào `%ProgramFiles%\print-util\`
- Đăng ký và khởi động **Windows Service** tự động (start cùng Windows)
- Tạo uninstaller trong Control Panel

### Build từ source

```powershell
git clone https://github.com/<user>/print-util
cd print-util
cargo build --release
```

Binary đầu ra: `target\release\print-util.exe`

### Build installer locally

Yêu cầu: [Inno Setup 6](https://jrsoftware.org/isdl.php)

```powershell
cargo build --release
iscc installer\setup.iss
# Output: installer\Output\print-util-0.1.0-setup.exe
```

### Bundle Ghostscript DLL (tuỳ chọn, khuyến nghị)

```powershell
# Copy DLL vào thư mục installer\vendor\ trước khi chạy iscc
New-Item -ItemType Directory -Force installer\vendor
Copy-Item "C:\Program Files\gs\gs10.04.0\bin\gsdll64.dll" installer\vendor\
```

> **Lưu ý license:** `gsdll64.dll` là AGPL-3.0. Không commit file này vào repo.

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

### `POST /print`

In một file PDF.

**Request:** `multipart/form-data`

| Field | Bắt buộc | Mô tả |
|-------|----------|-------|
| `file` | ✓ | Nội dung file PDF |
| `printer` | — | Tên máy in. Bỏ trống = dùng máy in mặc định |

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
| 400 | Thiếu field hoặc dữ liệu không hợp lệ |
| 500 | Lỗi khi gửi lệnh in |

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

Release installers are distributed via **Windows Package Manager (winget)** and validated by Microsoft — no SmartScreen warning for users.

See [CODE_SIGNING_POLICY.md](CODE_SIGNING_POLICY.md) for full details.

## License

MIT
