import { invoke } from "@tauri-apps/api/core";
export interface Connection {
  endpoint: string | null;
  cloudflared: string | null;
  registryUrl: string | null;
  registryEnabled: boolean;
  uuid: string | null;
}
export interface Device {
  id: string;
  name: string;
  revoked: boolean;
  pending: boolean;
  registrationRequest: string | null;
  retained?: number;
  pendingError?: string | null;
}
export interface NetworkSettings { schema: number; address: string; port: number; }
export interface Status {
  listener: string;
  localEndpoint: string | null;
  revision: string;
  uptimeSeconds: number;
  connection: Connection;
  connectionState: { mode: string; publication: string };
  tunnel: { phase: string; endpoint: string | null; error: string | null; logs: string[] };
  publication: { phase: string; error: string | null };
  storage: {
    measuredAt: number | null;
    totalBytes: number | null;
    availableBytes: number | null;
    dataBytes: number;
    databaseBytes: number;
    temporaryBytes: number;
    otherBytes: number;
    error: string | null;
  };
  devices: Device[];
  defaultRegistryUrl: string | null;
}
export interface Startup {
  registered: boolean;
  enabled: boolean;
  actionMatches: boolean;
}
export type PlatformStatus = Pick<Environment, "startup" | "startupError" | "trayStartup" | "updateSchedule" | "updateScheduleError">;
export interface Environment {
  registrationError?: string | null;
  network: NetworkSettings;
  platform: string;
  cloudflared: string;
  dataDir: string;
  startup: Startup | null;
  startupError: string | null;
  trayStartup: boolean;
  updateSettings: { schema: string; policy: "automatic" | "notify" | "off" };
  updateStatus: {
    schema: string;
    phase: string;
    targetVersion: string | null;
    lastCheckedAt: number | null;
    lastCompletedAt: number | null;
    deferredUntil: number | null;
    reason: string | null;
    lastFailedVersion: string | null;
  };
  updateSchedule: Startup | null;
  updateScheduleError: string | null;
}
export interface UpdateCheckOutcome {
  result: "skipped" | "current" | "available" | "deferred" | "started" | "completed";
  value?: string;
}
export interface Backend {
  status(): Promise<Status>;
  mutate(path: string, body: Record<string, unknown>): Promise<unknown>;
  environment(): Promise<Omit<Environment, keyof PlatformStatus>>;
  platformStatus(): Promise<PlatformStatus>;
  start(): Promise<void>;
  network(settings: NetworkSettings): Promise<void>;
  startup(action: "install" | "remove"): Promise<Startup>;
  trayStartup(enabled: boolean): Promise<void>;
  updatePolicy(policy: "automatic" | "notify" | "off"): Promise<void>;
  updateCheck(automatic: boolean): Promise<UpdateCheckOutcome>;
  uninstall(deleteData: boolean): Promise<void>;
  requestId(): Promise<string>;
  qr(uri: string): Promise<string>;
}
export const native: Backend = {
  status: () => invoke("manager_status"),
  mutate: (path, body) => invoke("manager_mutate", { path, body }),
  environment: () => invoke("manager_environment"),
  platformStatus: () => invoke("manager_platform_status"),
  start: () => invoke("manager_start"),
  network: (settings) => invoke("manager_network", { settings }),
  startup: (action) => invoke("manager_startup", { action }),
  trayStartup: (enabled) => invoke("manager_tray_startup", { enabled }),
  updatePolicy: (policy) => invoke("manager_update_policy", { policy }),
  updateCheck: (automatic) => invoke("manager_update_check", { automatic }),
  uninstall: (deleteData) => invoke("manager_uninstall", { deleteData }),
  requestId: () => invoke("manager_request_id"),
  qr: (uri) => invoke("manager_qr", { uri }),
};
export function formatBytes(n: number | null): string {
  if (n === null) return "측정 중";
  if (n >= 1024 ** 3) return `${(n / 1024 ** 3).toFixed(2)} GiB`;
  if (n >= 1024 ** 2) return `${(n / 1024 ** 2).toFixed(1)} MiB`;
  return `${n.toLocaleString()} B`;
}
export function updatePhase(value: string): string {
  return (
    (
      {
        idle: "대기 중",
        checking: "확인 중",
        downloading: "다운로드 중",
        "waiting-idle": "적용 대기 중",
        draining: "서버 종료 중",
        installing: "설치 중",
        restarting: "다시 시작 중",
        completed: "완료",
        deferred: "연기됨",
        failed: "실패",
      } as Record<string, string>
    )[value] ?? "상태를 확인하지 못했습니다."
  );
}
export function phase(value: string): string {
  return (
    (
      {
        connected: "연결됨",
        starting: "준비 중",
        failed: "오류 발생",
        stopped: "중지됨",
        external: "사용 안 함",
        published: "게시 완료",
        publishing: "게시 중",
        pending: "게시 대기 중",
        waiting: "대기 중",
        disabled: "사용 안 함",
        "waiting-for-address": "주소 준비 중",
      } as Record<string, string>
    )[value] ?? "상태를 확인하지 못했습니다."
  );
}
export function message(error: unknown): string {
  const code =
    typeof error === "string"
      ? error.split(":")[0]
      : "management-request-failed";
  const known: Record<string, string> = {
    "removal-installation-not-writable": "관리 앱을 쓰기 가능한 Applications 폴더로 옮긴 뒤 다시 실행하세요.",
    "removal-unowned-profile-files": "관리 앱 데이터 폴더에 별도로 저장한 파일이 있습니다. 해당 파일을 다른 폴더로 옮긴 뒤 다시 시도하세요.",
    "data-dir-busy": "다른 서버가 같은 데이터 폴더를 사용하고 있습니다. 해당 서버를 종료한 뒤 다시 시도하세요.",
    "incompatible-store": "서버 데이터 형식이 현재 버전과 호환되지 않습니다.",
    "unsafe-storage-path": "데이터 폴더에 링크가 포함되어 있습니다. 데이터 폴더 경로를 확인하세요.",
    "not-initialized": "서버 데이터가 초기화되지 않았습니다. 서버 설치 상태를 확인하세요.",
    "managed-registration-needs-directory": "임시 주소로 기기를 등록하려면 주소 레지스트리를 사용하세요.",
    "directory-unreachable": "레지스트리 서버에 연결하지 못했습니다. 네트워크와 레지스트리 서버 주소를 확인하세요.",
    "directory-unavailable": "레지스트리 서버를 사용할 수 없습니다. 잠시 후 다시 시도하세요.",
    "directory-full": "레지스트리 서버의 저장 공간이 부족합니다. 잠시 후 다시 시도하세요.",
    "directory-rejected": "레지스트리 서버가 게시 요청을 거부했습니다. 연결 설정을 확인하세요.",
    "directory-record-owned": "다른 서버가 사용 중인 등록 정보입니다. 연결 설정을 확인하세요.",
    "directory-admission-limited": "레지스트리 서버의 요청 한도를 초과했습니다. 잠시 후 다시 시도하세요.",

    "removal-shared-installation-or-data": "다른 서버가 같은 설치 경로나 데이터 폴더를 사용하고 있습니다. 해당 서버의 실행 등록을 먼저 정리하세요.",
    "removal-registration-missing": "설치 정보를 확인하지 못했습니다. 설치 상태를 확인한 뒤 다시 시도하세요.",
    "removal-registration-conflict": "설치 정보가 일치하지 않습니다. 설치 경로와 데이터 폴더를 확인하세요.",
    "removal-unowned-data-files": "데이터 폴더에 별도로 저장한 파일이 있습니다. 해당 파일을 다른 폴더로 옮긴 뒤 다시 시도하세요.",
    "removal-unowned-install-files": "설치 폴더에 별도로 저장한 파일이 있습니다. 해당 파일을 다른 폴더로 옮긴 뒤 다시 시도하세요.",
    "removal-linked-path": "제거할 경로에 링크가 포함되어 있습니다. 설치 경로와 데이터 폴더를 확인하세요.",
    "removal-close-other-managers": "다른 관리 화면을 종료한 뒤 다시 시도하세요.",
    "removal-process-still-running": "서버와 관리 화면을 종료한 뒤 다시 시도하세요.",
    "removal-data-failed": "데이터를 모두 삭제하지 못했습니다. 파일 사용 여부와 폴더 권한을 확인한 뒤 다시 시도하세요.",
    "removal-profile-failed": "관리 앱 데이터를 삭제하지 못했습니다. 관리 화면을 종료한 뒤 다시 시도하세요.",
    "update-recovery-required": "중단된 업데이트를 복구한 뒤 다시 시도하세요.",
    "update-recovery-schedule-mismatch": "관리 앱을 다시 실행한 뒤 업데이트를 다시 확인하세요.",
    "invalid-network-settings": "바인딩 IP 주소와 포트(1~65535)를 확인하세요.",
    "listen-address-in-use": "주소와 포트를 이미 사용 중입니다. 네트워크 설정에서 포트를 변경하세요.",
    "listen-address-unavailable": "이 컴퓨터에 없는 IP 주소입니다. 네트워크 설정을 확인하세요.",
    "listen-permission-denied": "설정한 주소와 포트에서 서버를 실행할 권한이 없습니다. 네트워크 설정을 확인하세요.",
    "tunnel-start-failed": "cloudflared를 실행하지 못했습니다. 실행 파일과 출력 내용을 확인하세요.",
    "tunnel-exited": "cloudflared가 종료되었습니다. 출력 내용을 확인하세요.",
    "tunnel-readiness-timeout": "90초 안에 임시 주소 연결을 완료하지 못했습니다. 네트워크와 출력 내용을 확인하세요.",
    "tunnel-job-unavailable": "cloudflared 프로세스를 관리하지 못했습니다. 출력 내용을 확인하세요.",
    "tunnel-output-unavailable": "cloudflared 출력을 읽지 못했습니다.",
    "management-stale-state":
      "서버 상태가 변경되었습니다. 새로 확인한 뒤 다시 시도하세요.",
    "registration-already-issued":
      "이미 발급한 요청입니다. 기기 목록을 확인하세요. 링크를 잃었다면 해당 기기를 해제한 뒤 다시 등록하세요.",
    "public-endpoint-not-ready":
      "서버 주소가 준비되지 않았습니다. 연결 설정을 확인하세요.",
    "local-endpoint-unavailable":
      "이 컴퓨터에서 연결할 수 있는 주소가 아닙니다. 네트워크 설정에서 바인딩 IP 주소를 확인하세요.",
    "invalid-device-name":
      "기기 이름을 확인하세요. 1~80자이며 제어 문자는 사용할 수 없습니다.",
    "absolute-cloudflared-executable-required":
      "cloudflared 실행 파일의 절대 경로를 확인하세요.",
    "management-response-incomplete":
      "응답을 끝까지 받지 못했습니다. 기기 목록과 설정을 확인한 뒤 다시 시도하세요.",
    "task-scheduler-operation-failed":
      "작업 스케줄러 설정을 변경하지 못했습니다. 사용자 권한과 시스템 정책을 확인하세요.",
  };
  return (
    known[code] ?? "작업을 완료하지 못했습니다. 서버 상태와 설정을 확인하세요."
  );
}

export function updateReason(reason: string): string {
  return ({
    "update-recovery-schedule-mismatch": "관리 앱을 다시 실행한 뒤 업데이트를 다시 확인하세요.",
    "management-active": "관리 작업이 끝난 뒤 다시 확인하세요.",
    "management-app-open-or-install-locked": "다른 관리 앱을 닫거나 설치가 끝난 뒤 다시 확인하세요.",
    "server-busy": "서버 작업이 끝난 뒤 업데이트를 다시 확인합니다.",
    "update-network-unavailable": "네트워크 연결 후 업데이트를 다시 확인합니다.",
    "update-recovery-required": "중단된 업데이트를 복구한 뒤 다시 시도하세요.",
  } as Record<string, string>)[reason] ?? "업데이트를 완료하지 못했습니다.";
}
