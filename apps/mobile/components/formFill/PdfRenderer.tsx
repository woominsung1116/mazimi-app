import React, { useEffect, useRef, useState } from 'react';
import { View } from 'react-native';
import WebView, { type WebViewMessageEvent } from 'react-native-webview';

export interface RenderedPage { uri: string; width: number; height: number }

/*
 * Off-screen page renderer: pdf.js in a sandboxed WebView turns each page
 * into a JPEG data URL. The PDF bytes are posted in-process and never leave
 * the device: the page's CSP forbids every network request except the pinned
 * pdf.js scripts (subresource-integrity checked) and its CJK character maps.
 */
const PDFJS = 'https://cdn.jsdelivr.net/npm/pdfjs-dist@3.11.174';
const HTML = `<!doctype html><html><head><meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'unsafe-inline' https://cdn.jsdelivr.net; connect-src https://cdn.jsdelivr.net; img-src data: blob:; style-src 'unsafe-inline'; font-src data: blob:; worker-src blob:">
<script src="${PDFJS}/build/pdf.worker.min.js" integrity="sha384-SnzOobpRMLXZ52iJvZm/C0fYw0OQemTXzTjIsdsfMcrCtCEe9qgzxTd3RSklO5x2" crossorigin="anonymous"></script>
<script src="${PDFJS}/build/pdf.min.js" integrity="sha384-/1qUCSGwTur9vjf/z9lmu/eCUYbpOTgSjmpbMQZ1/CtX2v/WcAIKqRv+U1DUCG6e" crossorigin="anonymous"></script>
</head><body><script>
const post = m => window.ReactNativeWebView.postMessage(JSON.stringify(m));
async function render(job, b64, width) {
  try {
    const bin = atob(b64); const data = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) data[i] = bin.charCodeAt(i);
    const doc = await pdfjsLib.getDocument({ data, isEvalSupported: false, enableXfa: false, cMapUrl: '${PDFJS}/cmaps/', cMapPacked: true }).promise;
    post({ type: 'count', job, count: doc.numPages });
    for (let i = 1; i <= doc.numPages; i++) {
      const page = await doc.getPage(i);
      const base = page.getViewport({ scale: 1 });
      const viewport = page.getViewport({ scale: width / base.width });
      const canvas = document.createElement('canvas');
      canvas.width = Math.round(viewport.width); canvas.height = Math.round(viewport.height);
      const ctx = canvas.getContext('2d'); ctx.fillStyle = '#fff'; ctx.fillRect(0, 0, canvas.width, canvas.height);
      await page.render({ canvasContext: ctx, viewport }).promise;
      post({ type: 'page', job, index: i - 1, uri: canvas.toDataURL('image/jpeg', 0.85), width: canvas.width, height: canvas.height });
      page.cleanup(); canvas.width = 0; canvas.height = 0;
    }
    await doc.destroy();
    post({ type: 'done', job });
  } catch (e) { post({ type: 'error', job, message: String((e && e.message) || e).slice(0, 200) }); }
}
function onMessage(e) { try { const m = JSON.parse(e.data); if (m.type === 'render') render(m.job, m.data, m.width); } catch (_) {} }
window.addEventListener('message', onMessage); document.addEventListener('message', onMessage);
post(window.pdfjsLib ? { type: 'ready' } : { type: 'error', job: -1, message: 'renderer_unavailable' });
</script></body></html>`;

export interface RenderJob { id: number; base64: string }

/** Renders `job` and reports every page; `onError` when the renderer is unavailable (offline, blocked CDN). */
export default function PdfRenderer({ job, width, onPages, onError }: { job: RenderJob | null; width: number; onPages: (jobId: number, pages: RenderedPage[]) => void; onError: (jobId: number, message: string) => void }) {
  const ref = useRef<WebView>(null);
  const [ready, setReady] = useState(false);
  const pages = useRef<RenderedPage[]>([]);
  const sent = useRef<number | null>(null);
  useEffect(() => {
    if (!ready || !job || sent.current === job.id) return;
    sent.current = job.id; pages.current = [];
    ref.current?.postMessage(JSON.stringify({ type: 'render', job: job.id, data: job.base64, width: Math.round(Math.min(1400, Math.max(600, width * 2))) }));
  }, [ready, job, width]);
  function onMessage(event: WebViewMessageEvent) {
    let message: { type: string; job?: number; index?: number; uri?: string; width?: number; height?: number; message?: string };
    try { message = JSON.parse(event.nativeEvent.data); } catch { return; }
    if (message.type === 'ready') { setReady(true); return; }
    if (message.type === 'error') { onError(message.job ?? -1, message.message ?? 'render_failed'); return; }
    if (!job || message.job !== job.id) return; // a newer job replaced this one
    const dim = (v: unknown) => typeof v === 'number' && Number.isFinite(v) ? Math.min(4000, Math.max(1, v)) : 1;
    if (message.type === 'page' && typeof message.uri === 'string' && message.uri.startsWith('data:image/jpeg;base64,') && Number.isInteger(message.index) && message.index! >= 0 && message.index! < 50) {
      pages.current[message.index!] = { uri: message.uri, width: dim(message.width), height: dim(message.height) };
    } else if (message.type === 'done' && sent.current === job.id) {
      // One delivery per job; a duplicate 'done' must not replace pages with [].
      sent.current = -job.id; onPages(job.id, Array.from(pages.current)); pages.current = [];
    }
  }
  return <View pointerEvents="none" style={{ position: 'absolute', width: 1, height: 1, opacity: 0, overflow: 'hidden' }}>
    <WebView ref={ref} source={{ html: HTML }} originWhitelist={['*']} onMessage={onMessage}
      onShouldStartLoadWithRequest={request => request.url === 'about:blank'}
      onError={() => onError(-1, 'renderer_unavailable')}
      javaScriptEnabled allowFileAccess={false} allowFileAccessFromFileURLs={false} allowUniversalAccessFromFileURLs={false}
      incognito setSupportMultipleWindows={false} cacheEnabled={false} style={{ width: 1, height: 1 }} />
  </View>;
}
