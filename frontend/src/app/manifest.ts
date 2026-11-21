import type { MetadataRoute } from "next";

/** Lets people add Splee to their home screen or dock as an app, which iOS needs before it allows push notifications. */
export default function manifest(): MetadataRoute.Manifest {
  return {
    name: "Splee",
    short_name: "Splee",
    description: "Your AI sales agent: leads, replies and meetings.",
    start_url: "/app",
    scope: "/",
    display: "standalone",
    background_color: "#f6f5f3",
    theme_color: "#f6f5f3",
    icons: [
      { src: "/icons/icon-192.png", sizes: "192x192", type: "image/png", purpose: "any" },
      { src: "/icons/icon-512.png", sizes: "512x512", type: "image/png", purpose: "any" },
      { src: "/icons/maskable-512.png", sizes: "512x512", type: "image/png", purpose: "maskable" },
    ],
  };
}
