
export interface ServerDirectory {
  baseUrl: string;
  uuid: string;
  key: string;
}
export interface ServerConfig {
  directory?: ServerDirectory;
  endpoint: string;
  libraryId: string;
  deviceId: string;
  token: string;
}
export class ServerSyncError extends Error {
  /** False for a rejection the same request would receive again. Errors raised
   * here rather than by native are worth another attempt. */
  constructor(
    readonly code: string,
    readonly retryable = true,
  ) {
    super(code);
    this.name = "ServerSyncError";
  }
}
