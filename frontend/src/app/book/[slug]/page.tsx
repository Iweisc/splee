import type { Metadata } from "next";
import { BookingFlow } from "./booking-flow";

export const metadata: Metadata = { title: "Book a call", robots: { index: false } };

export default async function BookPage({ params }: PageProps<"/book/[slug]">) {
  const { slug } = await params;
  return <BookingFlow slug={slug} />;
}
