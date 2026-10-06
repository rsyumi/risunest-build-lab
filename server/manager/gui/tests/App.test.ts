import { mount, tick, unmount } from "svelte";
import { beforeEach, afterEach, expect, it, vi } from "vitest";
import App from "../src/App.svelte";
import type { Backend, Environment, PlatformStatus, Status } from "../src/api";

let component: ReturnType<typeof mount> | undefined;
let target: HTMLDivElement;
let snapshot: Status;
let environmentSnapshot: Environment;
let backend: Backend;
function platformSnapshot(): PlatformStatus {
  const { startup, startupError, trayStartup, updateSchedule, updateScheduleError } = structuredClone(environmentSnapshot);
  return { startup, startupError, trayStartup, updateSchedule, updateScheduleError };
}
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal(
    "fetch",
    vi.fn(() => {
      throw new Error("network prohibited in manager component tests");
    }),
  );
  target = document.createElement("div");
  document.body.append(target);
  snapshot = {
    listener: "127.0.0.1:14319",
    localEndpoint: "http://127.0.0.1:14319",
    revision: "synthetic:0",
    uptimeSeconds: 5,
    connection: {
      endpoint: "https://sync.example.com",
      cloudflared: null,
      registryUrl: "https://registry.example.com",
      registryEnabled: true,
      uuid: "synthetic-uuid",
    },
    connectionState: { mode: "fixed", publication: "published" },
    tunnel: { phase: "external", endpoint: null, error: null, logs: [] },
    publication: { phase: "published", error: null },
    storage: {
      measuredAt: 1,
      totalBytes: 100,
      availableBytes: 1000,
      dataBytes: 50,
      databaseBytes: 40,
      temporaryBytes: 5,
      otherBytes: 5,
      error: null,
    },
    devices: [],
    defaultRegistryUrl: "https://registry.example.com",
  };
  environmentSnapshot = {
    network: { schema: 1, address: "127.0.0.1", port: 14319 },
    platform: "windows",
    dataDir: "synthetic",
    cloudflared: "C:\\synthetic\\cloudflared.exe",
    startup: { registered: false, enabled: false, actionMatches: true },
    startupError: null,
    trayStartup: false,
    updateSettings: { schema: "risunest-sync-update-settings/v1", policy: "automatic" },
    updateStatus: {
      schema: "risunest-sync-update-status/v1",
      phase: "idle",
      targetVersion: null,
      lastCheckedAt: null,
      lastCompletedAt: null,
      deferredUntil: null,
      reason: null,
      lastFailedVersion: null,
    },
    updateSchedule: { registered: true, enabled: true, actionMatches: true },
    updateScheduleError: null,
  };
  backend = {
    status: vi.fn(async () => structuredClone(snapshot)),
    environment: vi.fn(async () => structuredClone(environmentSnapshot)),
    platformStatus: vi.fn(async () => platformSnapshot()),
    mutate: vi.fn(async () => ({})),
    start: vi.fn(async () => {}),
    network: vi.fn(async (settings) => { environmentSnapshot.network = settings; }),
    startup: vi.fn(async () => ({ registered: true, enabled: true, actionMatches: true })),
    trayStartup: vi.fn(async () => {}),
    updatePolicy: vi.fn(async () => {}),
    updateCheck: vi.fn(async () => ({ result: "current" })),
    uninstall: vi.fn(async () => {}),
    requestId: vi.fn(async () => "a".repeat(64)),
    qr: vi.fn(async () => '<svg aria-label="synthetic-qr"></svg>'),
  };
  HTMLDialogElement.prototype.showModal = function () {
    this.setAttribute("open", "");
  };
  HTMLDialogElement.prototype.close = function () {
    this.removeAttribute("open");
  };
});
afterEach(async () => {
  if (component) await unmount(component);
  component = undefined;
  target.remove();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});
async function settle() {
  await Promise.resolve();
  await tick();
  await Promise.resolve();
  await tick();
}
function button(text: string) {
  const result = [...target.querySelectorAll("button")].find(
    (b) => b.textContent?.trim() === text,
  );
  expect(result).toBeDefined();
  return result!;
}
async function open() {
  component = mount(App, { target, props: { backend } });
  await settle();
}

async function applyConnection(endpoint: string) {
  button("연결").click();
  await settle();
  const input = target.querySelector<HTMLInputElement>("input[type=url]")!;
  input.value = endpoint;
  input.dispatchEvent(new Event("input", { bubbles: true }));
  target
    .querySelector('form[aria-label="연결 설정"]')!
    .dispatchEvent(new Event("submit", { cancelable: true, bubbles: true }));
  await settle();
}

it("dismisses the applied notice after four seconds", async () => {
  await open();
  await applyConnection("https://changed.example.com");
  expect(target.textContent).toContain("변경 사항을 적용했습니다.");
  await vi.advanceTimersByTimeAsync(3999);
  expect(target.textContent).toContain("변경 사항을 적용했습니다.");
  await vi.advanceTimersByTimeAsync(1);
  expect(target.textContent).not.toContain("변경 사항을 적용했습니다.");
});

it("gives a repeated applied notice its own four seconds", async () => {
  await open();
  await applyConnection("https://first.example.com");
  await vi.advanceTimersByTimeAsync(2000);
  await applyConnection("https://second.example.com");
  expect(backend.mutate).toHaveBeenCalledTimes(2);
  await vi.advanceTimersByTimeAsync(2000);
  expect(target.textContent).toContain("변경 사항을 적용했습니다.");
  await vi.advanceTimersByTimeAsync(2000);
  expect(target.textContent).not.toContain("변경 사항을 적용했습니다.");
});

it("does not dismiss a later error with the applied notice timer", async () => {
  await open();
  await applyConnection("https://changed.example.com");
  await vi.advanceTimersByTimeAsync(2000);
  vi.mocked(backend.mutate).mockRejectedValue("management-stale-state");
  await applyConnection("https://retry.example.com");
  expect(target.textContent).toContain("서버 상태가 변경되었습니다. 새로 확인한 뒤 다시 시도하세요.");
  await vi.advanceTimersByTimeAsync(4000);
  expect(target.textContent).toContain("서버 상태가 변경되었습니다. 새로 확인한 뒤 다시 시도하세요.");
});

it("keeps a dirty form tied to its original revision during status refresh", async () => {
  await open();
  button("연결").click();
  await settle();
  const input = target.querySelector<HTMLInputElement>("input[type=url]")!;
  input.value = "https://changed.example.com";
  input.dispatchEvent(new Event("input", { bubbles: true }));
  snapshot.revision = "synthetic:1";
  await vi.advanceTimersByTimeAsync(3000);
  await settle();
  expect(input.value).toBe("https://changed.example.com");
  target
    .querySelector('form[aria-label="연결 설정"]')!
    .dispatchEvent(new Event("submit", { cancelable: true, bubbles: true }));
  await settle();
  expect(backend.mutate).toHaveBeenCalledWith(
    "connection",
    expect.objectContaining({
      revision: "synthetic:0",
      options: expect.objectContaining({
        endpoint: "https://changed.example.com",
      }),
    }),
  );
});

it("keeps an untouched connection form clean as the tunnel becomes ready", async () => {
  snapshot.connectionState.mode = "managed";
  snapshot.connection.cloudflared = "C:\\synthetic\\cloudflared.exe";
  snapshot.connection.endpoint = null;
  snapshot.tunnel = { phase: "starting", endpoint: null, error: null, logs: [] };
  await open();
  button("연결").click();
  await settle();
  expect(button("설정 적용").disabled).toBe(true);

  snapshot.connection.endpoint = "https://synthetic.trycloudflare.com";
  snapshot.tunnel = {
    phase: "connected",
    endpoint: "https://synthetic.trycloudflare.com",
    error: null,
    logs: ["registered"],
  };
  await vi.advanceTimersByTimeAsync(3000);
  await settle();

  expect(target.textContent).toContain("현재 설정입니다.");
  expect(target.textContent).not.toContain("아직 적용하지 않은 변경이 있습니다.");
  expect(button("설정 적용").disabled).toBe(true);
  button("개요").click();
  await settle();
  expect(target.querySelector("dialog[open]")).toBeNull();
});

it("rebases a dirty connection draft after an action from the same form", async () => {
  vi.mocked(backend.mutate).mockImplementation(async (path) => {
    if (path === "tunnel/restart") {
      snapshot.revision = "synthetic:1";
      return structuredClone(snapshot);
    }
    return {};
  });
  snapshot.connectionState.mode = "managed";
  snapshot.connection.cloudflared = "C:\\synthetic\\cloudflared.exe";
  await open();
  button("연결").click();
  await settle();
  const registry = target.querySelector<HTMLInputElement>(
    'input[aria-label="레지스트리 사용"]',
  )!;
  registry.click();
  button("다시 시작").click();
  await vi.waitFor(() =>
    expect(target.textContent).toContain("임시 주소 연결을 다시 시작했습니다."),
  );
  await settle();
  target
    .querySelector('form[aria-label="연결 설정"]')!
    .dispatchEvent(new Event("submit", { cancelable: true, bubbles: true }));
  await settle();
  expect(backend.mutate).toHaveBeenLastCalledWith(
    "connection",
    expect.objectContaining({ revision: "synthetic:1" }),
  );
});

it("labels retained overview data as last-known after losing the daemon", async () => {
  snapshot.connectionState.mode = "managed";
  snapshot.connection.cloudflared = "C:\\synthetic\\cloudflared.exe";
  snapshot.connection.endpoint = "https://synthetic.trycloudflare.com";
  snapshot.tunnel = {
    phase: "connected",
    endpoint: "https://synthetic.trycloudflare.com",
    error: null,
    logs: [],
  };
  await open();
  expect(target.textContent).toContain("서버 실행 중");
  vi.mocked(backend.status).mockRejectedValue("daemon-unavailable");
  await vi.advanceTimersByTimeAsync(3000);
  await settle();
  expect(target.textContent).toContain("마지막으로 확인한 서버 정보");
  expect(target.textContent).not.toContain("서버 실행 중");
  button("연결").click();
  await settle();
  expect(target.textContent).toContain("확인할 수 없음");
  expect(target.textContent).toContain("마지막 임시 주소");
  expect(target.textContent?.replace(/\s+/g, " ")).toContain(
    "서버를 중지하면 임시 주소 연결도 함께 종료됩니다.",
  );
});

it("shows a registration URI once and clears it on modal close", async () => {
  const uri = "risunestlocal://sync-server/register#SYNTHETIC_SECRET_ONLY_TEST";
  vi.mocked(backend.mutate).mockResolvedValue({ uri });
  await open();
  button("기기 등록").click();
  await settle();
  const name = target.querySelector<HTMLInputElement>(
    'input[aria-label="기기 이름"]',
  )!;
  name.value = "시험 기기";
  name.dispatchEvent(new Event("input", { bubbles: true }));
  target
    .querySelector("dialog form")!
    .dispatchEvent(new Event("submit", { cancelable: true, bubbles: true }));
  await settle();
  expect(backend.mutate).toHaveBeenCalledTimes(1);
  expect(backend.qr).toHaveBeenCalledWith(uri);
  await vi.waitFor(() =>
    expect(target.querySelector<HTMLTextAreaElement>("textarea")?.value).toBe(
      uri,
    ),
  );
  button("완료").click();
  await settle();
  expect(target.querySelector("textarea")).toBeNull();
  expect(target.innerHTML).not.toContain("SYNTHETIC_SECRET_ONLY_TEST");
});

it("disables issuance while disconnected and does not invent storage values", async () => {
  vi.mocked(backend.status).mockRejectedValue("daemon-unavailable");
  await open();
  expect(button("기기 등록").disabled).toBe(true);
  expect(target.textContent).toContain("서버 연결 안 됨");
  expect(target.textContent).not.toContain("1.28");
  expect(backend.mutate).not.toHaveBeenCalled();
});

it("distinguishes an initial measurement failure from a zero or pending measurement", async () => {
  snapshot.storage = {
    ...snapshot.storage,
    measuredAt: null,
    totalBytes: null,
    availableBytes: null,
    error: "storage-measurement-failed",
  };
  await open();
  expect(target.textContent).toContain("측정할 수 없음");
  expect(target.textContent).not.toContain("마지막 측정값");
});

it("changes the shared update policy from run settings", async () => {
  await open();
  button("실행 설정").click();
  await settle();
  const select = target.querySelector<HTMLSelectElement>(
    'select[aria-label="업데이트 정책"]',
  )!;
  expect(select.value).toBe("automatic");
  select.value = "notify";
  select.dispatchEvent(new Event("change", { bubbles: true }));
  await settle();
  expect(backend.updatePolicy).toHaveBeenCalledWith("notify");
});

it("refreshes a schedule failure on window focus without periodically querying services", async () => {
  environmentSnapshot.updateSchedule = null;
  environmentSnapshot.updateScheduleError = "update-already-running";
  await open();
  button("실행 설정").click();
  await settle();
  const select = target.querySelector<HTMLSelectElement>(
    'select[aria-label="업데이트 정책"]',
  )!;
  expect(target.textContent).toContain("예약 업데이트 상태를 확인하지 못했습니다.");
  expect(select.disabled).toBe(true);

  environmentSnapshot.updateSchedule = {
    registered: true,
    enabled: true,
    actionMatches: true,
  };
  environmentSnapshot.updateScheduleError = null;
  const platformReads = vi.mocked(backend.platformStatus).mock.calls.length;
  await vi.advanceTimersByTimeAsync(15_000);
  await settle();
  expect(target.textContent).toContain("예약 업데이트 상태를 확인하지 못했습니다.");
  expect(backend.platformStatus).toHaveBeenCalledTimes(platformReads);
  window.dispatchEvent(new Event("focus"));
  await settle();
  expect(target.textContent).not.toContain("예약 업데이트 상태를 확인하지 못했습니다.");
  expect(select.disabled).toBe(false);
  expect(backend.platformStatus).toHaveBeenCalledTimes(platformReads + 1);
});

it("keeps a dirty connection draft on its page and away from update actions", async () => {
  await open();
  button("연결").click();
  await settle();
  const input = target.querySelector<HTMLInputElement>("input[type=url]")!;
  input.value = "https://unsaved.example.com";
  input.dispatchEvent(new Event("input", { bubbles: true }));
  button("실행 설정").click();
  await settle();
  expect(target.querySelector<HTMLInputElement>("input[type=url]")?.value).toBe(
    "https://unsaved.example.com",
  );
  expect(target.querySelector("dialog[open] h2")?.textContent).toBe("페이지를 이동하시겠습니까?");
  expect(target.querySelector("dialog[open]")?.textContent).toContain("적용하지 않은 변경사항은 초기화됩니다.");
  button("취소").click();
  await settle();
  expect(input.value).toBe("https://unsaved.example.com");
  button("실행 설정").click();
  await settle();
  button("이동").click();
  await settle();
  expect(target.querySelector('form[aria-label="연결 설정"]')).toBeNull();
  button("연결").click();
  await settle();
  expect(target.querySelector<HTMLInputElement>("input[type=url]")?.value).toBe("https://sync.example.com");
  expect(backend.updateCheck).not.toHaveBeenCalled();
});

it("disables manual update checks while a dialog is open", async () => {
  await open();
  button("기기 등록").click();
  button("실행 설정").click();
  await settle();
  const check = button("업데이트 확인 및 적용");
  expect(check.disabled).toBe(true);
  check.click();
  expect(backend.updateCheck).not.toHaveBeenCalled();
});

it("reports a deferred manual update without claiming the app is current", async () => {
  vi.mocked(backend.updateCheck).mockResolvedValue({
    result: "deferred",
    value: "management-active",
  });
  await open();
  button("실행 설정").click();
  await settle();
  button("업데이트 확인 및 적용").click();
  await vi.waitFor(() =>
    expect(target.textContent).toContain("관리 작업이 끝난 뒤 다시 확인"),
  );
  expect(backend.updateCheck).toHaveBeenCalledWith(false);
  expect(target.textContent).not.toContain("현재 최신 버전입니다.");
});

it("coordinates an automatic update after the scheduled checker found the open GUI", async () => {
  environmentSnapshot.updateStatus.phase = "deferred";
  environmentSnapshot.updateStatus.reason =
    "management-app-open-or-install-locked";
  environmentSnapshot.updateStatus.targetVersion = "2.0.0";
  vi.mocked(backend.updateCheck).mockResolvedValue({
    result: "started",
    value: "2.0.0",
  });
  await open();
  await settle();
  expect(backend.updateCheck).toHaveBeenCalledTimes(1);
  expect(backend.updateCheck).toHaveBeenCalledWith(true);
  expect(target.textContent).toContain("관리 앱을 종료하고 적용");
});

it("does not hot-loop automatic handoff while another manager keeps replacement deferred", async () => {
  environmentSnapshot.updateStatus.phase = "deferred";
  environmentSnapshot.updateStatus.reason =
    "management-app-open-or-install-locked";
  environmentSnapshot.updateStatus.targetVersion = "2.0.0";
  vi.mocked(backend.updateCheck).mockResolvedValue({
    result: "deferred",
    value: "management-app-open-or-install-locked",
  });
  await open();
  await vi.waitFor(() => expect(backend.updateCheck).toHaveBeenCalledTimes(1));
  await vi.advanceTimersByTimeAsync(45_000);
  await settle();
  expect(backend.updateCheck).toHaveBeenCalledTimes(1);
});


it("saves network settings while the server is stopped and discards only confirmed drafts", async () => {
  vi.mocked(backend.status).mockRejectedValue("daemon-unavailable");
  await open();
  button("연결").click();
  await settle();
  const address = target.querySelector<HTMLInputElement>('input[aria-label="바인딩 주소"]')!;
  const port = target.querySelector<HTMLInputElement>('input[aria-label="포트"]')!;
  address.value = "0.0.0.0";
  address.dispatchEvent(new Event("input", { bubbles: true }));
  port.value = "24319";
  port.dispatchEvent(new Event("input", { bubbles: true }));
  await settle();
  button("개요").click();
  await settle();
  target.querySelector("dialog")!.dispatchEvent(new Event("cancel", { cancelable: true }));
  await settle();
  expect(address.value).toBe("0.0.0.0");
  address.closest("form")!.dispatchEvent(new Event("submit", { cancelable: true }));
  await vi.waitFor(() => expect(backend.network).toHaveBeenCalledWith({ schema: 1, address: "0.0.0.0", port: 24319 }));
  await settle();
  expect(target.textContent).toContain("다음 서버 시작부터 적용됩니다.");
  button("개요").click();
  await settle();
  expect(target.querySelector("dialog[open]")).toBeNull();
});

it("shows the last tunnel failure and output while retrying, as text", async () => {
  snapshot.tunnel = { phase: "starting", endpoint: null, error: "tunnel-exited", logs: ['<script>synthetic</script>', 'DNS lookup failed'] };
  await open();
  expect(target.textContent).toContain("cloudflared가 종료되었습니다.");
  button("연결").click();
  await settle();
  expect(target.textContent).toContain("tunnel-exited");
  expect(target.querySelector("pre")?.textContent).toContain("DNS lookup failed");
  expect(target.querySelector("pre script")).toBeNull();
});


it("preserves server data unless deletion is explicitly selected and confirmation is accepted", async () => {
  await open();
  button("실행 설정").click();
  await settle();
  button("제거").click();
  await settle();
  const dialog = target.querySelector("dialog")!;
  const checkbox = dialog.querySelector<HTMLInputElement>('input[type="checkbox"]')!;
  expect(checkbox.checked).toBe(false);
  button("취소").click();
  await settle();
  expect(backend.uninstall).not.toHaveBeenCalled();
  button("제거").click();
  await settle();
  const confirmation = [...dialog.querySelectorAll("button")].find(b => b.textContent?.trim() === "제거")!;
  confirmation.click();
  await settle();
  expect(backend.uninstall).toHaveBeenCalledWith(false);
});

it("passes the explicit data deletion choice and keeps removal errors visible", async () => {
  vi.mocked(backend.uninstall).mockRejectedValue("removal-shared-installation-or-data");
  await open();
  button("실행 설정").click();
  await settle();
  button("제거").click();
  await settle();
  const dialog = target.querySelector("dialog")!;
  const checkbox = dialog.querySelector<HTMLInputElement>('input[type="checkbox"]')!;
  checkbox.click();
  await settle();
  [...dialog.querySelectorAll("button")].find(b => b.textContent?.trim() === "제거")!.click();
  await settle();
  expect(backend.uninstall).toHaveBeenCalledWith(true);
  expect(dialog.hasAttribute("open")).toBe(true);
});

function localCheckbox() {
  return [...target.querySelectorAll<HTMLInputElement>("dialog input[type=checkbox]")].at(0);
}

it("issues a local registration only when the listener offers a loopback endpoint", async () => {
  const uri = "risunestlocal://sync-server/register#SYNTHETIC_LOCAL_ONLY_TEST";
  vi.mocked(backend.mutate).mockResolvedValue({ uri });
  await open();
  button("기기 등록").click();
  await settle();
  const name = target.querySelector<HTMLInputElement>(
    'input[aria-label="기기 이름"]',
  )!;
  name.value = "이 컴퓨터";
  name.dispatchEvent(new Event("input", { bubbles: true }));
  localCheckbox()!.click();
  await settle();
  target
    .querySelector("dialog form")!
    .dispatchEvent(new Event("submit", { cancelable: true, bubbles: true }));
  await settle();
  expect(backend.mutate).toHaveBeenCalledWith("devices", {
    revision: "synthetic:0",
    name: "이 컴퓨터",
    requestId: "a".repeat(64),
    target: "local",
  });
  await vi.waitFor(() =>
    expect(target.textContent?.replace(/\s+/g, " ")).toContain(
      "이 컴퓨터에서 실행하는 RisuNest 앱에만 등록할 수 있습니다.",
    ),
  );
  button("완료").click();
  await settle();
  expect(target.innerHTML).not.toContain("SYNTHETIC_LOCAL_ONLY_TEST");
});

it("hides the local option and keeps configured issuance for a specific listener", async () => {
  snapshot.listener = "192.0.2.1:14319";
  snapshot.localEndpoint = null;
  vi.mocked(backend.mutate).mockResolvedValue({ uri: "risunestlocal://sync-server/register#X" });
  await open();
  button("기기 등록").click();
  await settle();
  expect(localCheckbox()).toBeUndefined();
  const name = target.querySelector<HTMLInputElement>(
    'input[aria-label="기기 이름"]',
  )!;
  name.value = "다른 기기";
  name.dispatchEvent(new Event("input", { bubbles: true }));
  target
    .querySelector("dialog form")!
    .dispatchEvent(new Event("submit", { cancelable: true, bubbles: true }));
  await settle();
  expect(backend.mutate).toHaveBeenCalledWith("devices", {
    revision: "synthetic:0",
    name: "다른 기기",
    requestId: "a".repeat(64),
    target: "configured",
  });
  await vi.waitFor(() =>
    expect(target.textContent?.replace(/\s+/g, " ")).toContain(
      "등록할 기기의 RisuNest 앱에서 QR 코드를 스캔하거나",
    ),
  );
});

it("keeps slow environment and platform reads single flight without periodically querying services", async () => {
  let resolveEnvironment!: (value: Environment) => void;
  let resolvePlatform!: (value: PlatformStatus) => void;
  vi.mocked(backend.environment).mockImplementation(() => new Promise(resolve => { resolveEnvironment = resolve; }));
  vi.mocked(backend.platformStatus).mockImplementation(() => new Promise(resolve => { resolvePlatform = resolve; }));
  await open();
  button("실행 설정").click();
  await vi.advanceTimersByTimeAsync(60000);
  expect(backend.environment).toHaveBeenCalledTimes(1);
  expect(backend.platformStatus).toHaveBeenCalledTimes(1);
  resolveEnvironment(environmentSnapshot);
  resolvePlatform(platformSnapshot());
  await settle();
  await vi.advanceTimersByTimeAsync(60000);
  expect(backend.platformStatus).toHaveBeenCalledTimes(1);
});

it("renders registration conflicts and publication failures as actionable messages", async () => {
  environmentSnapshot.registrationError = "removal-registration-conflict";
  snapshot.publication = { phase: "failed", error: "directory-full" };
  await open();
  expect(target.textContent).toContain("설치 정보가 일치하지 않습니다.");
  button("연결").click();
  await settle();
  expect(target.textContent).toContain("레지스트리 서버의 저장 공간이 부족합니다.");
});

it("shows the last successful device sync and refreshes it without inventing a missing history", async () => {
  snapshot.devices = [
    { id: "synthetic-synced", name: "Synced", revoked: false, pending: false, registrationRequest: null, lastAck: 1790771696 },
    { id: "synthetic-unused", name: "Unused", revoked: false, pending: false, registrationRequest: null, lastAck: null },
  ];
  await open();
  button("기기").click();
  await settle();
  const rows = target.querySelectorAll(".device-row");
  expect(rows[0].textContent).toContain(`마지막 동기화: ${new Date("2026-09-30T12:34:56Z").toLocaleString()}`);
  expect(rows[0].textContent).not.toContain("동기화 기록 없음");
  expect(rows[1].textContent).toContain("동기화 기록 없음");
  expect(rows[1].textContent).not.toContain("마지막 동기화:");

  snapshot.devices[0].lastAck = 1790858096;
  await vi.advanceTimersByTimeAsync(3000);
  await settle();
  expect(target.querySelector(".device-row")?.textContent).toContain(`마지막 동기화: ${new Date("2026-10-01T12:34:56Z").toLocaleString()}`);
  expect(backend.mutate).not.toHaveBeenCalled();
});

it("requires confirmation before releasing revoked device custody", async () => {
  snapshot.devices = [{ id: "synthetic-device", name: "Synthetic", revoked: true, pending: false, registrationRequest: null, retained: 2, pendingError: "incompatible-store" }];
  await open();
  button("기기").click();
  await settle();
  expect(target.textContent).toContain("서버 데이터 형식이 현재 버전과 호환되지 않습니다.");
  button("보관 해제").click();
  await settle();
  expect(backend.mutate).not.toHaveBeenCalled();
  expect(target.querySelector("dialog[open]")?.textContent).toContain("삭제될 수 있습니다.");
  target.querySelector<HTMLButtonElement>("dialog[open] button.primary")!.click();
  await settle();
  expect(backend.mutate).toHaveBeenCalledWith("devices/synthetic-device/forget", expect.objectContaining({ revision: "synthetic:0" }));
});

it("renders unknown update reasons as a localized outcome with a separate diagnostic code", async () => {
  environmentSnapshot.updateStatus.reason = "synthetic-unknown-reason";
  await open();
  button("실행 설정").click();
  await settle();
  expect(target.textContent).toContain("업데이트를 완료하지 못했습니다.");
  expect(target.querySelector("code")?.textContent).toBe("synthetic-unknown-reason");
  expect(target.textContent).toContain("RisuNest Sync · 1.0.0");
});
