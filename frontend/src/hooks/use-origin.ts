import { useSyncExternalStore } from "react";

const subscribe = () => () => {};

/** The origin this page is served from, empty during the server render. The API lives on the same host as the app. */
export const useOrigin = () => useSyncExternalStore(subscribe, () => window.location.origin, () => "");
