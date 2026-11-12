const VERSION = 'triage-shell-v13';
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
    (async () => {
      const keys = await caches.keys();
      await Promise.all(keys.filter((key) => key !== VERSION).map((key) => caches.delete(key)));
      await self.clients.claim();
      // A tab running an older inbox cannot reload itself and would keep answering with dead
      // controls. Ask every tab once; reload the ones that do not answer.
      const clients = await self.clients.matchAll({ type: 'window', includeUncontrolled: true }),
        answered = new Set(),
        onMessage = (event) => {
          if (event.data?.type === 'triage-shell-pong') answered.add(event.source?.id);
        };
      self.addEventListener('message', onMessage);
      for (const client of clients) {
        try {
          client.postMessage({ type: 'triage-shell-ping' });
        } catch {}
      }
      await new Promise((resolve) => setTimeout(resolve, 1500));
      self.removeEventListener('message', onMessage);
      await Promise.all(
        clients.map((client) =>
          answered.has(client.id) ? null : client.navigate?.(client.url)?.catch(() => {}),
        ),
      );
    })(),
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
