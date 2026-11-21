import type { Metadata } from "next";
import { RoomView } from "./room-view";

// Each room is one buyer's, so it stays out of search engines.
export const metadata: Metadata = {
  title: "Deal room",
  robots: { index: false, follow: false },
};

export default async function RoomPage({ params, searchParams }: PageProps<"/room/[token]">) {
  const { token } = await params;
  const { preview } = await searchParams;
  return <RoomView token={token} preview={preview !== undefined} />;
}
