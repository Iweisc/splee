// Splee's service worker. It only shows push notifications and opens them. There's no fetch handler on purpose: nothing is
// cached, so every page and API call goes to the network as if this weren't here.

self.addEventListener("activate", (event) => {
  // Tabs that were open before it was installed can be sent to a notification's page too.
  event.waitUntil(self.clients.claim());
});

self.addEventListener("push", (event) => {
  let data;
  try {
    data = event.data ? event.data.json() : {};
  } catch {
    data = { body: event.data.text() };
  }
  event.waitUntil(
    self.registration.showNotification(data.title || "Splee", {
      body: data.body || "",
      icon: "/icons/icon-192.png",
      // A newer notification with the same tag replaces the old one, and still alerts.
      tag: data.tag || undefined,
      renotify: Boolean(data.tag),
      data: { url: data.url || "/app" },
    }),
  );
});

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  // Only ever a page of this app, whatever host the server put in the URL.
  const target = new URL(event.notification.data?.url || "/app", self.location.origin);
  const url = new URL(target.pathname + target.search + target.hash, self.location.origin).href;
  event.waitUntil(
    (async () => {
      const tabs = await self.clients.matchAll({ type: "window", includeUncontrolled: true });
      const tab = tabs.find((c) => new URL(c.url).origin === self.location.origin);
      if (tab) {
        await tab.focus();
        try {
          await tab.navigate(url);
          return;
        } catch {
          // A tab this worker doesn't control can't be navigated. Open the page beside it instead.
        }
      }
      await self.clients.openWindow(url);
    })(),
  );
});
