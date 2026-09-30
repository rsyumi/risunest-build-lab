import { endpointId, ProtocolError, readEnvelope, writerHash } from "./protocol";

function failure(code: string, status: number): Response {
  return Response.json(
    { error: code },
    { status, headers: status === 503 || status === 429 ? { "retry-after": "60" } : undefined },
  );
}

async function handle(request: Request, env: Env): Promise<Response> {
  const uuid = endpointId(new URL(request.url));
  if (request.method !== "GET" && request.method !== "POST") {
    const response = failure("method-not-allowed", 405);
    response.headers.set("allow", "GET, POST");
    return response;
  }
  if (request.method === "GET") {
    // Without the Sessions API, D1 reads go to the primary, including when
    // read replication is enabled. Do not replace this with a replica read.
    const row = await env.DB.prepare(
      "SELECT envelope FROM endpoints WHERE uuid = ?1 AND envelope IS NOT NULL",
    )
      .bind(uuid)
      .first<{ envelope: string }>();
    return row === null
      ? failure("not-found", 404)
      : new Response(row.envelope, {
          headers: { "content-type": "text/plain; charset=utf-8" },
        });
  }
  const writer = await writerHash(request);
  const envelope = await readEnvelope(request);
  const maxRecords = env.MAX_RECORDS;
  if (!Number.isInteger(maxRecords) || maxRecords < 1 || maxRecords > 10000)
    return failure("invalid-configuration", 500);

  const existing = await env.DB.prepare("SELECT uuid FROM endpoints WHERE uuid = ?1")
    .bind(uuid).first<{ uuid: string }>();
  if (existing === null) {
    const ip = request.headers.get("cf-connecting-ip");
    if (!ip) return failure("creation-origin-required", 403);
    const admission = await env.CREATION_LIMITER.limit({ key: ip });
    if (!admission.success) return failure("creation-rate-limited", 429);
  }

  // Admission and replacement are one atomic statement, so simultaneous new
  // UUIDs cannot race past the cap. CASE skips the count scan for updates.
  // A retry may rewrite identical bytes; it never creates a second record.
  const saved = await env.DB.prepare(
    `
    INSERT INTO endpoints (uuid, envelope, updated_at, writer)
    SELECT ?1, ?2, ?4, ?5
    WHERE CASE
      WHEN EXISTS (SELECT 1 FROM endpoints WHERE uuid = ?1) THEN 1
      ELSE (SELECT COUNT(*) FROM endpoints) < ?3
    END
    ON CONFLICT(uuid) DO UPDATE SET
      envelope = excluded.envelope, updated_at = excluded.updated_at
    WHERE endpoints.writer = excluded.writer
    RETURNING uuid
  `,
  )
    .bind(uuid, envelope, maxRecords, Date.now(), writer)
    .first<{ uuid: string }>();
  if (saved !== null) return new Response(null, { status: 204 });
  const claimed = await env.DB.prepare("SELECT uuid FROM endpoints WHERE uuid = ?1")
    .bind(uuid).first();
  return claimed === null ? failure("registry-full", 503) : failure("record-owned", 403);
}

export default {
  async scheduled(controller: ScheduledController, env: Env): Promise<void> {
    const cutoff = controller.scheduledTime - 30 * 24 * 60 * 60 * 1000;
    try {
      // Keep ownership after expiry so old link holders cannot claim the UUID.
      await env.DB.prepare("UPDATE endpoints SET envelope = NULL WHERE updated_at <= ?1 AND envelope IS NOT NULL")
        .bind(cutoff)
        .run();
    } catch {
      // Report a failed invocation without exposing D1 exceptions or row data.
      throw new Error("endpoint-cleanup-failed");
    }
  },
  async fetch(request: Request, env: Env): Promise<Response> {
    let response: Response;
    try {
      response = await handle(request, env);
    } catch (error) {
      // D1 errors can contain SQL or bound data. Never echo or log the exception.
      response =
        error instanceof ProtocolError
          ? failure(error.code, error.status)
          : failure("storage-unavailable", 503);
    }
    response.headers.set("cache-control", "no-store");
    response.headers.set("x-content-type-options", "nosniff");
    return response;
  },
} satisfies ExportedHandler<Env>;
