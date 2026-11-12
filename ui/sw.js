const VERSION = 'triage-shell-v11';
const SHELL = [
  '/',
  '/style.css',
  '/app.js',
  '/marked.js',
  '/manifest.webmanifest',
  '/apple-touch-icon.png',
  '/fonts/Geist-Variable.woff2',
  '/fonts/GeistMono-Variable.woff2',
];

self.addEventListener('install', (e) => {
  e.waitUntil(caches.open(VERSION).then((cache) => cache.addAll(SHELL)).then(() => self.skipWaiting()));
});

self.addEventListener('activate', (e) => {
  e.waitUntil(
    caches.keys().then((keys) => Promise.all(keys.filter((key) => key !== VERSION).map((key) => caches.delete(key))))
      .then(() => self.clients.claim()),
  );
});

self.addEventListener('fetch', (e) => {
  const url = new URL(e.request.url);
  if (e.request.method !== 'GET' || url.origin !== self.location.origin || url.pathname.startsWith('/v1/')) return;

  // Network-first: a deploy shows up on the first load, the cache only serves offline.
  const update = caches.open(VERSION).then((cache) =>
    fetch(e.request)
      .then((response) => {
        if (response.ok) cache.put(e.request, response.clone());
        return response;
      })
      .catch(() => cache.match(e.request).then((cached) => cached || Response.error()))
  );
  e.respondWith(update);
});
