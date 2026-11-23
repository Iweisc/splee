import { useSyncExternalStore } from "react";

const subscribe = () => () => {};

/**
 * A value only the browser can read, like the page's address: `onServer` during the server render and hydration, then
 * `read()`. `read` must return the same value each call, so a primitive, not a new object.
 */
export const useBrowserValue = <T>(read: () => T, onServer: T): T => useSyncExternalStore(subscribe, read, () => onServer);
