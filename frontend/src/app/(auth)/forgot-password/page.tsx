import type { Metadata } from "next";
import { ForgotForm } from "./forgot-form";

export const metadata: Metadata = { title: "Reset your password · Splee" };

export default function ForgotPasswordPage() {
  return <ForgotForm />;
}
