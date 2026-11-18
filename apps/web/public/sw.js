const CACHE_NAME = "wello-v1";
const STATIC_ASSETS = [
  "/",
  "/manifest.json",
];

// 캐시할 API 경로 (네트워크 우선, 폴백 캐시)
// v2: 인증·사용자 응답이 섞여 있던 v1 캐시는 activate 시 삭제된다.
const API_CACHE_NAME = "wello-api-v2";
const API_BASE = "/api/";

// 오프라인 폴백을 허용하는 공개 카탈로그 경로만 캐시한다. 그 외 /api/* 는
// (/api/v1/dashboard·my·alerts·profile·admin 등 사용자 응답)
// 로그인 주체에 묶인 데이터이므로 Cache Storage 에 저장하지 않는다.
const PUBLIC_API_PATTERNS = [
  /^\/api\/v1\/programs(\/[^/]+)?$/,
  /^\/api\/v1\/campus\//,
  /^\/api\/v1\/brands(\/[^/]+)?$/,
];

function isCacheablePublicApi(request, url) {
  if (request.method !== "GET") return false;
  if (request.headers.has("authorization")) return false;
  if (url.pathname.startsWith("/api/auth/")) return false;
  return PUBLIC_API_PATTERNS.some((pattern) => pattern.test(url.pathname));
}

// 설치: 정적 자산 사전 캐시
self.addEventListener("install", (event) => {
  event.waitUntil(
    caches
      .open(CACHE_NAME)
      .then((cache) => cache.addAll(STATIC_ASSETS))
      .then(() => self.skipWaiting())
  );
});

// 활성화: 이전 캐시 정리
self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) =>
        Promise.all(
          keys
            .filter((key) => key !== CACHE_NAME && key !== API_CACHE_NAME)
            .map((key) => caches.delete(key))
        )
      )
      .then(() => self.clients.claim())
  );
});

// 로그아웃 시 페이지가 보내는 메시지: 공개 카탈로그 캐시도 함께 비운다.
// 캐시 이름은 이 파일만 알고 있다. 페이지는 메시지만 보낸다.
self.addEventListener("message", (event) => {
  if (event.data && event.data.type === "clear-api-cache") {
    event.waitUntil(caches.delete(API_CACHE_NAME));
  }
});

// Fetch: 전략별 캐싱
self.addEventListener("fetch", (event) => {
  const { request } = event;
  const url = new URL(request.url);

  // 외부 도메인은 캐싱하지 않음
  if (url.origin !== self.location.origin) return;

  // 로그인·세션 라우트(/auth/*)와 인증 프록시(/bff/*)는 계정에 묶인 응답이므로
  // 서비스워커가 가로채거나 저장하지 않는다.
  if (url.pathname.startsWith("/auth/") || url.pathname.startsWith("/bff/")) return;

  // API 요청
  if (url.pathname.startsWith(API_BASE)) {
    // 인증·사용자 응답은 항상 네트워크로만 처리한다 (캐시 저장·폴백 없음).
    if (!isCacheablePublicApi(request, url)) return;
    // 공개 카탈로그: 네트워크 우선, 실패 시 캐시 폴백
    event.respondWith(
      fetch(request)
        .then((response) => {
          if (response.ok) {
            const clone = response.clone();
            caches.open(API_CACHE_NAME).then((cache) => cache.put(request, clone));
          }
          return response;
        })
        .catch(() => caches.match(request))
    );
    return;
  }

  // 정적 자산: 캐시 우선, 네트워크 폴백
  if (
    request.destination === "style" ||
    request.destination === "script" ||
    request.destination === "image" ||
    request.destination === "font"
  ) {
    event.respondWith(
      caches.match(request).then((cached) => {
        if (cached) return cached;
        return fetch(request).then((response) => {
          if (response.ok) {
            const clone = response.clone();
            caches.open(CACHE_NAME).then((cache) => cache.put(request, clone));
          }
          return response;
        });
      })
    );
    return;
  }

  // HTML 페이지: 네트워크 우선, 오프라인 시 캐시
  if (request.destination === "document" || request.mode === "navigate") {
    event.respondWith(
      fetch(request)
        .then((response) => {
          if (response.ok) {
            const clone = response.clone();
            caches.open(CACHE_NAME).then((cache) => cache.put(request, clone));
          }
          return response;
        })
        .catch(() =>
          caches.match(request).then((cached) => cached || caches.match("/"))
        )
    );
  }
});
