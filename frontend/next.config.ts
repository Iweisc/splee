import type { NextConfig } from "next";

const API_URL = process.env.API_URL ?? "http://127.0.0.1:8787";

const nextConfig: NextConfig = {
  // Same-origin proxy so the session cookie set by the Rust API works without CORS.
  // MCP clients look for the OAuth metadata at the public origin's root, which the API serves under /api.
  async rewrites() {
    return [
      { source: "/api/:path*", destination: `${API_URL}/api/:path*` },
      // Email tracking's short links and pixel, on PUBLIC_URL and every tracking host CNAMEd to it. See backend/src/tracking.rs.
      { source: "/c/:token", destination: `${API_URL}/api/t/c/:token` },
      { source: "/o/:file", destination: `${API_URL}/api/t/o/:file` },
      { source: "/.well-known/oauth-authorization-server", destination: `${API_URL}/api/.well-known/oauth-authorization-server` },
      { source: "/.well-known/oauth-protected-resource/:path*", destination: `${API_URL}/api/.well-known/oauth-protected-resource/:path*` },
    ];
  },
  // A page that frames the consent page could trick someone into clicking Allow.
  async headers() {
    return [{ source: "/oauth/:path*", headers: [{ key: "Content-Security-Policy", value: "frame-ancestors 'none'" }] }];
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
