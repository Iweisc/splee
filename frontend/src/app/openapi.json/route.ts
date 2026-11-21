import { openapi, originOf } from "@/lib/api-spec";

// Any origin may read it, so browser tools like Swagger Editor can load the spec by its URL.
export function GET(request: Request) {
  return Response.json(openapi(originOf(request)), { headers: { "Access-Control-Allow-Origin": "*" } });
}
