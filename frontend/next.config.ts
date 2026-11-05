import type { NextConfig } from "next";

const API_URL = process.env.API_URL ?? "http://127.0.0.1:8787";

const nextConfig: NextConfig = {
  // Same-origin proxy so the session cookie set by the Rust API works without CORS.
  async rewrites() {
    return [{ source: "/api/:path*", destination: `${API_URL}/api/:path*` }];
  },
  images: {
    remotePatterns: [{ protocol: "https", hostname: "www.google.com", pathname: "/s2/favicons/**" }],
  },
  turbopack: {
    rules: {
      "*.css": {
        loaders: ["@tailwindcss/turbopack"],
        as: "*.css",
      },
    },
  },
};

export default nextConfig;
