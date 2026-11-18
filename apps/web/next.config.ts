import type { NextConfig } from "next";

// Report-only for now: browsers log violations to the console and block
// nothing. The service worker is registered from /sw-register.js, so it needs
// no hash or nonce. Next.js's own inline bootstrap scripts (self.__next_f)
// differ per page, so script-src 'self' alone would block them. To enforce:
// add src/proxy.ts that makes a random nonce per request, sets
// `script-src 'self' 'nonce-<n>' 'strict-dynamic'` in the request's
// Content-Security-Policy header (Next.js then stamps the nonce on its
// scripts) and the response's Content-Security-Policy header, then drop the
// Report-Only header here. Nonces need per-request rendering, so static
// pages become dynamic. Also allow https://cdn.jsdelivr.net in script-src
// and connect-src (and worker-src if its worker is used): the admin PDF
// designer (admin/programs/[id]/FormTemplateDesigner.tsx) loads pdf.js and
// its cmaps from there.
const contentSecurityPolicy = [
  "default-src 'self'",
  "script-src 'self'",
  "style-src 'self' 'unsafe-inline'",
  "img-src 'self' data: blob: https:",
  "font-src 'self' data:",
  "connect-src 'self'",
  "worker-src 'self'",
  "manifest-src 'self'",
  "object-src 'none'",
  "base-uri 'self'",
  "form-action 'self'",
  "frame-ancestors 'none'",
].join("; ");

const nextConfig: NextConfig = {
  output: "standalone",
  poweredByHeader: false,
  // The app renders no next/image, so nothing needs the image optimizer.
  // With unoptimized set, `next start` answers every /_next/image request
  // with 404 before reading its parameters, so the optimizer's request
  // handling (fetch, decode, resize) is unreachable from outside.
  images: { unoptimized: true },
  async headers() {
    return [
      {
        source: "/:path*",
        headers: [
          {
            key: "Content-Security-Policy-Report-Only",
            value: contentSecurityPolicy,
          },
        ],
      },
      {
        // 서비스워커가 전체 도메인에 접근할 수 있도록 설정
        source: "/sw.js",
        headers: [
          {
            key: "Cache-Control",
            value: "public, max-age=0, must-revalidate",
          },
          {
            key: "Service-Worker-Allowed",
            value: "/",
          },
        ],
      },
      {
        // manifest.json 캐싱
        source: "/manifest.json",
        headers: [
          {
            key: "Cache-Control",
            value: "public, max-age=3600",
          },
        ],
      },
    ];
  },
};

export default nextConfig;
