// Which Splee user this browser's push subscription belongs to. A subscription follows the browser, not the sign-in, so when
// someone else signs in here the old one is dropped instead of carrying the first person's leads to them.
const OWNER_KEY = "splee_push_user";

export function markPushOwner(userId: string) {
  localStorage.setItem(OWNER_KEY, userId);
}

/** Unsubscribes this browser, so the push service tells the server it's gone on its next push. */
export async function dropPushHere() {
  localStorage.removeItem(OWNER_KEY);
  if (!("serviceWorker" in navigator)) return;
  try {
    const reg = await navigator.serviceWorker.getRegistration("/");
    await (await reg?.pushManager.getSubscription())?.unsubscribe();
  } catch {
    // Nothing to drop, or the browser has no push.
  }
}

/** Drops this browser's subscription when it was made by someone other than `userId`. True when it still belongs to them. */
export async function keepPushFor(userId: string) {
  if (localStorage.getItem(OWNER_KEY) === userId) return true;
  await dropPushHere();
  return false;
}
