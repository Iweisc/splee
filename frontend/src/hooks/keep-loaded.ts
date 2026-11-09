import { unstable_serialize, useSWRConfig, type Middleware } from "swr";
import { ApiError } from "@/lib/api";

/**
 * A background refresh that can't reach Splee, or hits a server error, keeps showing what already loaded instead of
 * swapping the page for an error, and SWR retries on its own. The error still shows when this key has never loaded,
 * like a new filter, and for a 4xx like a 404, since that answer is real.
 */
export const keepLoadedOnError: Middleware = (useSWRNext) => (key, fetcher, config) => {
  const swr = useSWRNext(key, fetcher, config);
  const { cache } = useSWRConfig();
  return {
    get data() {
      return swr.data;
    },
    get error() {
      const e = swr.error;
      const answered = e instanceof ApiError && e.status >= 400 && e.status < 500;
      return e && !answered && cache.get(unstable_serialize(key))?.data !== undefined ? undefined : e;
    },
    get isLoading() {
      return swr.isLoading;
    },
    get isValidating() {
      return swr.isValidating;
    },
    mutate: swr.mutate,
  };
};
