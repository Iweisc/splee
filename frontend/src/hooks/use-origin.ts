import { useBrowserValue } from "./use-browser-value";

const origin = () => window.location.origin;

/** The origin this page is served from, empty during the server render. The API lives on the same host as the app. */
export const useOrigin = () => useBrowserValue(origin, "");
