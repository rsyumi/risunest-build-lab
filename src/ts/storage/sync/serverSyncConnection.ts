import { ServerSyncError } from "./serverSync";

/** Mirrors native ServerConfig / sync-wire identity validation. Native bind
 * remains authoritative and performs authentication only after confirmation. */
export function validateServerSyncId(value: string): string {
  if (!/^[a-zA-Z0-9_-]{1,128}$/.test(value)) {
    throw new ServerSyncError("invalid-id");
  }
  return value;
}

export function validateServerSyncEndpoint(value: string): string {
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    throw new ServerSyncError("invalid-endpoint");
  }
  if (
    !url.hostname ||
    url.username ||
    url.password ||
    /^[^:]+:\/\/[^/]*@/.test(value) ||
    value.includes("?") ||
    value.includes("#")
  ) {
    throw new ServerSyncError("invalid-endpoint");
  }
  if (url.protocol !== "https:" && url.protocol !== "http:") {
    throw new ServerSyncError("invalid-endpoint");
  }
  url.pathname = `${url.pathname.replace(/\/+$/, "")}/`;
  return url.href;
}
