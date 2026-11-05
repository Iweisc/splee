"use client";

import useSWR from "swr";
import { api, ApiError } from "@/lib/api";
import type { Me } from "@/lib/types";

/** Current session. `me` is null when signed out (401), undefined while loading. */
export function useMe() {
  const { data, error, isLoading, mutate } = useSWR<Me | null>("/auth/me", async (path: string) => {
    try {
      return await api<Me>(path);
    } catch (e) {
      if (e instanceof ApiError && e.status === 401) return null;
      throw e;
    }
  });
  return { me: data, error, isLoading, mutate };
}

/** SWR fetcher for the Rust API. Usage: useSWR<T>("/projects", fetcher). */
export const fetcher = <T,>(path: string) => api<T>(path);
