const serverInput   = document.getElementById('server');
const pdfUrlInput   = document.getElementById('pdf-url');
const printerSelect = document.getElementById('printer-select');
const jobNameInput  = document.getElementById('job-name');
const logEl         = document.getElementById('log');

// ── log helpers ───────────────────────────────────────────────────────────────

function log(msg, cls = '') {
  const span = document.createElement('span');
  span.className = cls;
  span.textContent = msg + '\n';
  logEl.appendChild(span);
  logEl.scrollTop = logEl.scrollHeight;
}

function ts() {
  return new Date().toTimeString().slice(0, 8);
}

document.getElementById('btn-clear').addEventListener('click', () => {
  logEl.innerHTML = '';
});

// ── /health ───────────────────────────────────────────────────────────────────

document.getElementById('btn-health').addEventListener('click', async () => {
  const base = serverInput.value.trim().replace(/\/$/, '');
  log(`[${ts()}] GET ${base}/health …`, 'inf');
  try {
    const t0 = performance.now();
    const res = await fetch(`${base}/health`, { signal: AbortSignal.timeout(3000) });
    const ms = (performance.now() - t0).toFixed(0);
    const body = await res.json();
    if (res.ok) {
      log(`[${ts()}] ✓ ${res.status} ${JSON.stringify(body)}  (${ms}ms)`, 'ok');
    } else {
      log(`[${ts()}] ✗ HTTP ${res.status}`, 'err');
    }
  } catch (e) {
    log(`[${ts()}] ✗ ${e.message}`, 'err');
    log(`        → Server chưa chạy? Kiểm tra: print-util.exe đã start chưa`, 'err');
  }
});

// ── load printer list ────────────────────────────────────────────────────────

async function loadPrinters(silent = false) {
  const base = serverInput.value.trim().replace(/\/$/, '');
  if (!silent) log(`[${ts()}] GET ${base}/printers …`, 'inf');
  try {
    const res = await fetch(`${base}/printers`, { signal: AbortSignal.timeout(4000) });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    const body = await res.json();
    const printers = body.printers ?? [];
    const defaultPrinter = body.default ?? null;

    // Rebuild select options
    printerSelect.innerHTML = '<option value="">(mặc định)</option>';
    printers.forEach((name) => {
      const opt = document.createElement('option');
      opt.value = name;
      opt.textContent = name;
      if (name === defaultPrinter) opt.textContent += ' ★';
      printerSelect.appendChild(opt);
    });

    // Select the default printer automatically
    if (defaultPrinter && printers.includes(defaultPrinter)) {
      printerSelect.value = defaultPrinter;
    }

    if (!silent) log(`[${ts()}] ✓ ${printers.length} máy in (mặc định: ${defaultPrinter ?? 'không xác định'})`, 'ok');
  } catch (e) {
    if (!silent) log(`[${ts()}] ✗ ${e.message}`, 'err');
  }
}

document.getElementById('btn-reload-printers').addEventListener('click', () => loadPrinters(false));


// ── /print/a4 và /print/a5 ───────────────────────────────────────────────────

const btnA4 = document.getElementById('btn-print-a4');
const btnA5 = document.getElementById('btn-print-a5');

function setPrinting(active) {
  btnA4.disabled = active;
  btnA5.disabled = active;
  btnA4.style.opacity = active ? '0.5' : '';
  btnA5.style.opacity = active ? '0.5' : '';
}

async function doPrint(size) {
  if (btnA4.disabled) return; // already printing
  const base     = serverInput.value.trim().replace(/\/$/, '');
  const pdfUrl   = pdfUrlInput.value.trim();
  const printer  = printerSelect.value || undefined;
  const endpoint = `${base}/print/${size}`;
  const jobName  = jobNameInput.value.trim() || undefined;

  if (!pdfUrl) {
    log(`[${ts()}] ✗ Nhập URL file PDF trước.`, 'err');
    return;
  }

  setPrinting(true);
  log(`[${ts()}] Đang tải PDF: ${pdfUrl}`, 'inf');
  let blob;
  try {
    const res = await fetch(pdfUrl);
    if (!res.ok) throw new Error(`HTTP ${res.status} ${res.statusText}`);
    blob = await res.blob();
    log(`[${ts()}] ✓ Tải xong (${(blob.size / 1024).toFixed(1)} KB)`, 'ok');
  } catch (e) {
    log(`[${ts()}] ✗ Không tải được PDF: ${e.message}`, 'err');
    log(`        → Kiểm tra URL, CORS, hoặc file có tồn tại không`, 'err');
    setPrinting(false);
    return;
  }

  const form = new FormData();
  form.append('file', blob, 'document.pdf');
  if (printer) form.append('printer', printer);
  if (jobName) form.append('name', jobName);

  log(`[${ts()}] POST ${endpoint}${printer ? ` (printer="${printer}")` : ' (mặc định)'}${jobName ? ` name="${jobName}"` : ''} …`, 'inf');
  try {
    const t0  = performance.now();
    const res = await fetch(endpoint, {
      method: 'POST',
      body: form,
      signal: AbortSignal.timeout(130000),
    });
    const ms   = (performance.now() - t0).toFixed(0);
    const body = await res.json().catch(() => ({}));

    if (res.ok) {
      log(`[${ts()}] ✓ In ${size.toUpperCase()} thành công! (${ms}ms)`, 'ok');
    } else {
      log(`[${ts()}] ✗ HTTP ${res.status}: ${body.error ?? JSON.stringify(body)}`, 'err');
    }
  } catch (e) {
    log(`[${ts()}] ✗ ${e.message}`, 'err');
  } finally {
    setPrinting(false);
  }
}

document.getElementById('btn-print-a4').addEventListener('click', () => doPrint('a4'));
document.getElementById('btn-print-a5').addEventListener('click', () => doPrint('a5'));

// ── prefill PDF URL từ tab đang mở ────────────────────────────────────────────

chrome.tabs.query({ active: true, currentWindow: true }, ([tab]) => {
  if (tab?.url && (tab.url.includes('.pdf') || tab.url.startsWith('blob:'))) {
    pdfUrlInput.value = tab.url;
  }
});

// ── lưu server URL và auto-load printers ───────────────────────────────

chrome.storage.local.get({ serverUrl: 'http://127.0.0.1:17474' }, ({ serverUrl }) => {
  serverInput.value = serverUrl;
  loadPrinters(true);   // silent auto-load on open
});
serverInput.addEventListener('change', () => {
  chrome.storage.local.set({ serverUrl: serverInput.value.trim() });
  loadPrinters(true);
});
