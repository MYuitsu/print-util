# Extension Integration Guide

Tài liệu này dành cho developer viết browser extension (Chrome/Edge/Firefox) muốn tích hợp với **print-util** để in PDF ngầm từ trình duyệt.

## Tổng quan

```
Browser Extension
  │
  ├─ fetch() PDF về dạng Blob (hoặc dùng URL trực tiếp)
  │
  └─ POST multipart/form-data ──→ http://127.0.0.1:3000/print
                                         │
                                   print-util server
                                         │
                                   in qua Windows spooler (ngầm)
```

## Yêu cầu manifest

Extension cần khai báo quyền truy cập localhost:

### Manifest V3 (Chrome/Edge)

```json
{
  "permissions": ["activeTab"],
  "host_permissions": [
    "http://127.0.0.1/*"
  ]
}
```

### Manifest V2 (Firefox)

```json
{
  "permissions": [
    "activeTab",
    "http://127.0.0.1/*"
  ]
}
```

---

## API endpoints

### `GET /health`

Kiểm tra server có đang chạy không. Nên gọi trước khi in để hiện thông báo lỗi thân thiện nếu server chưa khởi động.

```
GET http://127.0.0.1:3000/health
```

Response:
```json
{ "status": "ok" }
```

---

### `POST /print`

Gửi PDF để in ngầm.

```
POST http://127.0.0.1:3000/print
Content-Type: multipart/form-data
```

**Fields:**

| Field | Type | Bắt buộc | Mô tả |
|-------|------|----------|-------|
| `file` | binary | ✓ | Nội dung file PDF |
| `printer` | string | — | Tên máy in. Bỏ trống = máy in mặc định của Windows |

**Response thành công (`200`):**
```json
{ "status": "ok" }
```

**Response lỗi (`400` / `500`):**
```json
{ "error": "mô tả lỗi" }
```

---

## Triển khai trong service worker

Đây là hàm tham khảo có thể dùng thẳng trong `background.js`:

```js
const PRINT_SERVER = 'http://127.0.0.1:3000';

/**
 * Kiểm tra server có sẵn sàng không.
 * @returns {Promise<boolean>}
 */
async function isServerAvailable() {
  try {
    const res = await fetch(`${PRINT_SERVER}/health`, { signal: AbortSignal.timeout(2000) });
    return res.ok;
  } catch {
    return false;
  }
}

/**
 * Fetch URL rồi in ngầm.
 * @param {string} pdfUrl    - URL của file PDF
 * @param {string} [printer] - Tên máy in (tuỳ chọn)
 * @returns {Promise<{ ok: boolean, error?: string }>}
 */
async function silentPrint(pdfUrl, printer) {
  // 1. Fetch PDF
  let blob;
  try {
    const res = await fetch(pdfUrl);
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    blob = await res.blob();
  } catch (e) {
    return { ok: false, error: `Không tải được PDF: ${e.message}` };
  }

  // 2. Gửi đến server
  const form = new FormData();
  form.append('file', blob, 'document.pdf');
  if (printer) form.append('printer', printer);

  try {
    const res = await fetch(`${PRINT_SERVER}/print`, { method: 'POST', body: form });
    if (!res.ok) {
      const body = await res.json().catch(() => ({}));
      return { ok: false, error: body.error ?? `Server lỗi ${res.status}` };
    }
    return { ok: true };
  } catch (e) {
    return { ok: false, error: `Server không phản hồi: ${e.message}` };
  }
}
```

### Ví dụ gắn vào context menu

```js
chrome.contextMenus.create({
  id: 'silent-print',
  title: 'In ngầm',
  contexts: ['link', 'page'],
});

chrome.contextMenus.onClicked.addListener(async (info, tab) => {
  const url = info.linkUrl ?? info.pageUrl;
  if (!url) return;

  const available = await isServerAvailable();
  if (!available) {
    // Hiện notification hoặc badge thông báo server chưa chạy
    chrome.action.setBadgeText({ text: '!', tabId: tab.id });
    return;
  }

  const result = await silentPrint(url);
  if (!result.ok) console.error('[print-util]', result.error);
});
```

---

## Lấy tên máy in có sẵn

Server không cung cấp endpoint liệt kê máy in. Để lấy danh sách, người dùng có thể mở PowerShell và chạy:

```powershell
Get-Printer | Select-Object -ExpandProperty Name
```

Extension có thể cho người dùng nhập tên máy in vào một ô `<input>` trong popup và lưu vào `chrome.storage.local`.

---

## Lưu ý bảo mật

- Server chỉ lắng nghe trên `127.0.0.1`, không nhận kết nối từ bên ngoài.
- Extension chỉ nên gọi `127.0.0.1`, không nên cho phép cấu hình địa chỉ tuỳ ý để tránh SSRF.
- Nên kiểm tra URL PDF thuộc nguồn tin cậy trước khi fetch & in, đặc biệt nếu extension tự động in theo trigger.

---

## Kiểm tra tích hợp nhanh

Mở DevTools Console trong extension background page rồi chạy:

```js
// Kiểm tra server
fetch('http://127.0.0.1:3000/health').then(r => r.json()).then(console.log);

// In thử một PDF public
silentPrint('https://www.w3.org/WAI/WCAG21/wcag21.pdf').then(console.log);
```
