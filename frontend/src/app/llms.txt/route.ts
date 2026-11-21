import { llmsTxt, originOf } from "@/lib/api-spec";

export function GET(request: Request) {
  return new Response(llmsTxt(originOf(request)), { headers: { "Content-Type": "text/plain; charset=utf-8" } });
}
