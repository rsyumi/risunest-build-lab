import type { languageEnglish } from "src/lang/en";
import { formatElapsed } from "../../gui/nativeFileJobDialogModel";
import { formatRisuNestStorageBytes } from "../risuNestStorageDashboard";
import type { AssetResidencyPolicy } from "./serverAssetResidency";
import type { ServerConfig, ServerSyncProgress } from "./serverSync";
import { serverSyncBlocked, type ServerSyncSnapshot } from "./serverSyncController";

/** The strings both connection screens draw from. */
export type ServerSyncText = (typeof languageEnglish)["risuNest"]["serverSync"];

export interface ServerSyncConnectRequest {
  config: ServerConfig;
  residency: AssetResidencyPolicy;
  /** Replace this library's device credentials instead of binding a new server. */
  replacing?: boolean;
}
export interface ServerSyncConnectPort {
  bind(config: ServerConfig, prepare?: () => Promise<unknown>): Promise<void>;
  reregister(config: ServerConfig, prepare?: () => Promise<unknown>): Promise<void>;
  synchronize(): Promise<void>;
}

/**
 * Registers the device, stores the asset policy, then runs the first sync.
 * The policy is written after binding because native refuses a remote policy
 * without a bound server, and before the first cycle because the engine reads
 * it at sync time: a remote device then receives asset metadata instead of
 * every asset body.
 */
export async function connectServerSync(
  controller: ServerSyncConnectPort,
  setAssetResidencyPolicy: (policy: AssetResidencyPolicy) => Promise<unknown>,
  request: ServerSyncConnectRequest,
): Promise<void> {
  const prepare = () => setAssetResidencyPolicy(request.residency);
  if (request.replacing) await controller.reregister(request.config, prepare);
  else await controller.bind(request.config, prepare);
  await controller.synchronize();
}

export function serverSyncRefreshRequired(error: string): boolean {
  return (
    error === "committed-refresh-pending" ||
    error === "activation-confirmation-pending"
  );
}

/** The sentence shown for a failed action or attempt. A rejection the same
 * attempt would receive again says what to do instead of asking for a retry. */
export function serverSyncErrorHelp(
  code: string,
  text: ServerSyncText,
  retryable = true,
): string {
  switch (code) {
    case "activation-confirmation-pending":
      return text.activationHelp;
    case "committed-refresh-pending":
      return text.refreshHelp;
    case "library-operation-busy":
      return text.busyHelp;
    case "device-credential-unavailable":
      return text.credentialUnavailable;
    case "server-incompatible":
      return text.incompatibleHelp;
    default:
      return retryable ? text.errorHelp : text.blockedHelp;
  }
}

export type ServerSyncStatusTone =
  | "idle"
  | "connected"
  | "paused"
  | "working"
  | "attention";
export interface ServerSyncStatusView {
  label: string;
  tone: ServerSyncStatusTone;
}

/** The one-line state shown beside the section title. */
export function serverSyncStatus(
  snapshot: ServerSyncSnapshot,
  text: ServerSyncText,
  actionError = "",
): ServerSyncStatusView {
  const error = actionError || snapshot.error;
  if (snapshot.status?.registrationRequired)
    return { label: text.registrationRequired, tone: "attention" };
  if (serverSyncRefreshRequired(error))
    return { label: text.refreshPending, tone: "attention" };
  if (snapshot.result?.phase === "conflict")
    return { label: text.conflict, tone: "attention" };
  if (snapshot.running)
    return {
      label: snapshot.progress ? text.progress[snapshot.progress] : text.running,
      tone: "working",
    };
  if (snapshot.paused) return { label: text.paused, tone: "paused" };
  if (error === "server-incompatible")
    return { label: text.incompatible, tone: "attention" };
  if (serverSyncBlocked(snapshot))
    return { label: text.blocked, tone: "attention" };
  if (snapshot.status?.operationPending)
    return { label: text.pending, tone: "connected" };
  if (snapshot.status?.configured && snapshot.status.fullScan)
    return { label: text.initialScan, tone: "connected" };
  if (snapshot.status?.configured)
    return { label: text.ready, tone: "connected" };
  return { label: text.disconnected, tone: "idle" };
}
/** The local changes waiting to be sent. Both connection screens show this. */
export function serverSyncPendingChanges(
  snapshot: ServerSyncSnapshot,
  text: ServerSyncText,
): string {
  return snapshot.status
    ? fill(text.count, formatCount(snapshot.status.dirtyRecords))
    : "";
}

export const SERVER_SYNC_STAGES: readonly ServerSyncProgress[] = [
  "saving",
  "preparing",
  "applying",
  "refreshing",
  "publishing",
];
export type ServerSyncStageState = "done" | "active" | "pending";
export interface ServerSyncStageView {
  stage: ServerSyncProgress;
  label: string;
  state: ServerSyncStageState;
  detail: string;
}
export interface ServerSyncCounterView {
  key: "bytes" | "rate" | "items" | "pending";
  label: string;
  value: string;
}
export interface ServerSyncProgressView {
  /** Share of this cycle's records processed, or null before the count is known. */
  percent: number | null;
  /** The active stage, with its record count when there is one. */
  current: string;
  elapsed: string;
  stages: ServerSyncStageView[];
  counters: ServerSyncCounterView[];
}

const NO_VALUE = "-";
const formatCount = (value: number): string => value.toLocaleString();
const fill = (template: string, value: string): string =>
  template.replace("{0}", value);

/** Everything a progress panel or summary row shows during an attempt. */
export function serverSyncProgressView(
  snapshot: ServerSyncSnapshot,
  text: ServerSyncText,
  now: number,
): ServerSyncProgressView {
  const items = snapshot.cycleItems;
  const counted = items !== undefined && items.total > 0;
  const activeIndex = snapshot.progress
    ? SERVER_SYNC_STAGES.indexOf(snapshot.progress)
    : 0;
  const ratio = counted ? `${formatCount(items.done)} / ${formatCount(items.total)}` : "";
  const stages = SERVER_SYNC_STAGES.map((stage, index): ServerSyncStageView => {
    const state: ServerSyncStageState =
      index < activeIndex ? "done" : index === activeIndex ? "active" : "pending";
    let detail = "";
    if (counted && stage === "preparing" && state === "done")
      detail = fill(text.itemsCount, formatCount(items.total));
    else if (
      counted &&
      state === "active" &&
      (stage === "applying" || stage === "publishing")
    )
      detail = ratio;
    return { stage, label: text.progress[stage], state, detail };
  });
  const active = stages[activeIndex];
  const activity = snapshot.progress === "preparing" || snapshot.progress === "publishing" ? items?.activity : undefined;
  const activityCount = items?.expected ? `${formatCount(items.processed ?? 0)} / ${formatCount(items.expected)}` : formatCount(items?.processed ?? 0);
  const bytes =
    snapshot.verifiedBytes === undefined
      ? NO_VALUE
      : formatRisuNestStorageBytes(Number(snapshot.verifiedBytes));
  const rate =
    snapshot.bytesPerSecond === undefined
      ? NO_VALUE
      : `${formatRisuNestStorageBytes(snapshot.bytesPerSecond)}/s`;
  const pending = serverSyncPendingChanges(snapshot, text) || NO_VALUE;
  return {
    percent: counted && activity !== "confirming" ? Math.min(100, Math.round((items.done / items.total) * 100)) : null,
    current: activity ? `${text.activity[activity]}${activity === "confirming" ? "" : ` · ${activityCount}`}` : active.detail ? `${active.label} · ${active.detail}` : active.label,
    elapsed:
      snapshot.attemptStartedAt === undefined
        ? ""
        : `${text.elapsed} ${formatElapsed(now - (snapshot.phaseStartedAt ?? snapshot.attemptStartedAt))}`,
    stages,
    counters: [
      { key: "bytes", label: text.verifiedBytes, value: bytes },
      { key: "rate", label: text.transferRate, value: rate },
      { key: "items", label: text.progressItems, value: counted ? ratio : NO_VALUE },
      { key: "pending", label: text.pendingChanges, value: pending },
    ],
  };
}

/** The server address without its scheme, for a compact chip. */
export function serverSyncHostLabel(endpoint: string): string {
  try {
    const url = new URL(endpoint);
    return `${url.host}${url.pathname.replace(/\/+$/, "")}`;
  } catch {
    return endpoint;
  }
}
