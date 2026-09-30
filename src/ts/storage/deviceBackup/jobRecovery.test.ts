import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => {
  class NeedsAttention extends Error {
    constructor(
      readonly code: string,
      readonly jobId: string,
      readonly warningCodes: string[] = [],
      readonly committedResult?: unknown,
    ) {
      super(code);
      this.name = "PortableExportNeedsAttention";
    }
  }
  return {
    NeedsAttention,
    invoke: vi.fn(),
    alertSelect: vi.fn(),
    alertNormal: vi.fn(),
    resume: vi.fn(),
    remember: vi.fn(),
    store: {
      read: vi.fn(),
      write: vi.fn(),
      clear: vi.fn(),
    },
    publication: {
      androidAcknowledgementPending: vi.fn(),
      acknowledgeAndroid: vi.fn(),
    },
  };
});

vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("../../platform", () => ({ isTauriIOS: false }));
vi.mock("../../alert", () => ({
  alertSelect: mocks.alertSelect,
  alertNormal: mocks.alertNormal,
}));
vi.mock("./job", async (importOriginal) => ({
  ...await importOriginal<typeof import("./job")>(),
  PortableExportNeedsAttention: mocks.NeedsAttention,
  createPortableExportIntentStore: () => mocks.store,
  portableAndroidPublicationDependencies: () => mocks.publication,
  rememberPortableExport: mocks.remember,
  resumePendingPortableExport: mocks.resume,
}));

import { language } from "../../../lang";
import { resumePortableExportsAfterBootstrap } from "./jobRecovery";

const job = {
  jobId: "portable-job-1",
  kind: "export-portable-backup",
  state: "succeeded",
  phase: "complete",
  progress: { completedBytes: 8, completedItems: 1 },
  result: {
    handoffPath: "C:\\synthetic\\backup.risunest",
    warningCodes: [],
  },
};

describe("portable export bootstrap recovery", () => {
  let intent: Record<string, unknown> | null;

  beforeEach(() => {
    vi.clearAllMocks();
    (window as Window & { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ = {};
    intent = {
      jobId: job.jobId,
      phase: "waiting-native",
      publication: "desktop-path",
    };
    mocks.store.read.mockImplementation(() => intent);
    mocks.store.write.mockImplementation((next) => {
      intent = next;
    });
    mocks.store.clear.mockImplementation(() => {
      intent = null;
    });
    mocks.invoke.mockImplementation(async (command: string) => {
      if (command === "native_file_job_list") return [job];
      if (command === "native_file_job_status") return job;
      if (
        command === "native_portable_handoff_cleanup"
        || command === "native_file_job_forget"
      ) return undefined;
      throw new Error(`Unexpected command: ${command}`);
    });
    mocks.publication.androidAcknowledgementPending.mockResolvedValue(false);
    mocks.publication.acknowledgeAndroid.mockResolvedValue(true);
  });

  it("allows deferring a running unrecorded job after one minute without cancellation", async () => {
    vi.useFakeTimers();
    try {
      intent = null;
      const running = { ...job, state: "running", result: undefined };
      mocks.invoke.mockImplementation(async (command: string) => command === "native_file_job_list" ? [running] : running);
      mocks.alertSelect.mockResolvedValueOnce("0").mockResolvedValueOnce("1");
      const pending = resumePortableExportsAfterBootstrap();
      await vi.advanceTimersByTimeAsync(60_100);
      await pending;
      expect(mocks.alertSelect).toHaveBeenLastCalledWith([language.portableBackup.recovery.wait, language.portableBackup.recovery.later], language.portableBackup.recovery.stillRunning);
      expect(mocks.invoke.mock.calls.some(([command]) => command === "native_file_job_cancel" || command === "native_file_job_forget")).toBe(false);
      expect(mocks.remember).not.toHaveBeenCalled();
    } finally { vi.useRealTimers(); }
  });

  it("does not call native recovery commands during web bootstrap", async () => {
    delete (window as Window & { __TAURI_INTERNALS__?: unknown })
      .__TAURI_INTERNALS__;

    await resumePortableExportsAfterBootstrap();

    expect(mocks.invoke).not.toHaveBeenCalled();
    expect(mocks.resume).not.toHaveBeenCalled();
  });

  it("discards a succeeded export and both native receipts when selected", async () => {
    mocks.resume.mockRejectedValueOnce(
      new mocks.NeedsAttention("publication-failed", job.jobId),
    );
    mocks.alertSelect.mockResolvedValueOnce("1");

    await resumePortableExportsAfterBootstrap();

    expect(mocks.invoke).toHaveBeenCalledWith("native_portable_handoff_cleanup", {
      path: job.result.handoffPath,
    });
    expect(mocks.invoke).toHaveBeenCalledWith("native_file_job_forget", {
      jobId: job.jobId,
    });
    expect(mocks.store.clear).toHaveBeenCalledWith(job.jobId);
  });

  it("settles the prior Android receipt before retrying publication", async () => {
    intent = {
      jobId: job.jobId,
      phase: "publishing",
      publication: "android-saf",
      requestId: "request-1",
    };
    mocks.publication.androidAcknowledgementPending.mockResolvedValue(true);
    mocks.resume
      .mockRejectedValueOnce(new mocks.NeedsAttention("copy-failed", job.jobId))
      .mockImplementationOnce(async () => {
        intent = null;
      });
    mocks.alertSelect.mockResolvedValueOnce("0");

    await resumePortableExportsAfterBootstrap();

    expect(mocks.publication.acknowledgeAndroid).toHaveBeenCalledWith("request-1");
    expect(mocks.store.write).toHaveBeenCalledWith(expect.objectContaining({
      jobId: job.jobId,
      phase: "waiting-native",
      requestId: undefined,
    }));
    expect(mocks.resume).toHaveBeenCalledTimes(2);
  });

  it("retains the job when an active Android receipt cannot be acknowledged", async () => {
    intent = {
      jobId: job.jobId,
      phase: "publishing",
      publication: "android-saf",
      requestId: "request-1",
    };
    mocks.publication.androidAcknowledgementPending.mockResolvedValue(true);
    mocks.publication.acknowledgeAndroid.mockResolvedValue(false);
    mocks.resume.mockRejectedValueOnce(
      new mocks.NeedsAttention("copy-failed", job.jobId),
    );
    mocks.alertSelect.mockResolvedValueOnce("1").mockResolvedValueOnce("0");

    await resumePortableExportsAfterBootstrap();

    expect(mocks.invoke).not.toHaveBeenCalledWith(
      "native_portable_handoff_cleanup",
      expect.anything(),
    );
    expect(mocks.invoke).not.toHaveBeenCalledWith(
      "native_file_job_forget",
      expect.anything(),
    );
    expect(mocks.store.clear).not.toHaveBeenCalled();
    expect(mocks.alertSelect).toHaveBeenLastCalledWith(
      [language.portableBackup.recovery.later],
      language.portableBackup.recovery.recoveryFailed,
    );
  });

  it.each(["0", "1"])("preserves Android recovery ownership while receipt settlement is pending (%s)", async (choice) => {
    intent = { jobId: job.jobId, phase: "publishing", publication: "android-saf", requestId: "request-1" };
    let settle!: (value: boolean) => void;
    mocks.publication.acknowledgeAndroid.mockImplementationOnce(() => new Promise<boolean>((resolve) => { settle = resolve; }));
    mocks.publication.androidAcknowledgementPending.mockResolvedValue(true);
    mocks.resume.mockRejectedValueOnce(new mocks.NeedsAttention("copy-failed", job.jobId));
    mocks.alertSelect.mockResolvedValueOnce(choice).mockResolvedValueOnce("0");
    const recovering = resumePortableExportsAfterBootstrap();
    await vi.waitFor(() => expect(mocks.publication.acknowledgeAndroid).toHaveBeenCalled());
    expect(mocks.store.write).not.toHaveBeenCalled();
    expect(mocks.store.clear).not.toHaveBeenCalled();
    expect(mocks.invoke).not.toHaveBeenCalledWith("native_portable_handoff_cleanup", expect.anything());
    expect(mocks.invoke).not.toHaveBeenCalledWith("native_file_job_forget", expect.anything());
    settle(false);
    await recovering;
    expect(mocks.store.write).not.toHaveBeenCalled();
    expect(mocks.store.clear).not.toHaveBeenCalled();
    expect(intent?.requestId).toBe("request-1");
  });

  it("waits for the pending receipt check before retaining the Android export", async () => {
    intent = { jobId: job.jobId, phase: "publishing", publication: "android-saf", requestId: "request-1" };
    let settle!: (value: boolean) => void;
    mocks.publication.acknowledgeAndroid.mockResolvedValueOnce(false);
    mocks.publication.androidAcknowledgementPending.mockImplementationOnce(() => new Promise<boolean>((resolve) => { settle = resolve; }));
    mocks.resume.mockRejectedValueOnce(new mocks.NeedsAttention("copy-failed", job.jobId));
    mocks.alertSelect.mockResolvedValueOnce("1").mockResolvedValueOnce("0");
    const recovering = resumePortableExportsAfterBootstrap();
    await vi.waitFor(() => expect(mocks.publication.androidAcknowledgementPending).toHaveBeenCalled());
    expect(mocks.store.write).not.toHaveBeenCalled();
    expect(mocks.store.clear).not.toHaveBeenCalled();
    expect(mocks.invoke).not.toHaveBeenCalledWith("native_portable_handoff_cleanup", expect.anything());
    settle(true);
    await recovering;
    expect(mocks.store.clear).not.toHaveBeenCalled();
    expect(intent?.requestId).toBe("request-1");
  });

  it("discards the handoff only after asynchronous acknowledgement succeeds", async () => {
    intent = { jobId: job.jobId, phase: "publishing", publication: "android-saf", requestId: "request-1" };
    let settle!: (value: boolean) => void;
    mocks.publication.acknowledgeAndroid.mockImplementationOnce(() => new Promise<boolean>((resolve) => { settle = resolve; }));
    mocks.resume.mockRejectedValueOnce(new mocks.NeedsAttention("copy-failed", job.jobId));
    mocks.alertSelect.mockResolvedValueOnce("1");
    const recovering = resumePortableExportsAfterBootstrap();
    await vi.waitFor(() => expect(mocks.publication.acknowledgeAndroid).toHaveBeenCalled());
    expect(mocks.invoke).not.toHaveBeenCalledWith("native_portable_handoff_cleanup", expect.anything());
    expect(mocks.store.clear).not.toHaveBeenCalled();
    settle(true);
    await recovering;
    expect(mocks.invoke).toHaveBeenCalledWith("native_portable_handoff_cleanup", { path: job.result.handoffPath });
    expect(mocks.store.clear).toHaveBeenCalledWith(job.jobId);
  });

  it("retains ownership when Android acknowledgement rejects", async () => {
    intent = { jobId: job.jobId, phase: "publishing", publication: "android-saf", requestId: "request-1" };
    mocks.publication.acknowledgeAndroid.mockRejectedValueOnce(new Error("bridge unavailable"));
    mocks.resume.mockRejectedValueOnce(new mocks.NeedsAttention("copy-failed", job.jobId));
    mocks.alertSelect.mockResolvedValueOnce("1").mockResolvedValueOnce("0");
    await resumePortableExportsAfterBootstrap();
    expect(mocks.store.write).not.toHaveBeenCalled();
    expect(mocks.store.clear).not.toHaveBeenCalled();
    expect(mocks.invoke).not.toHaveBeenCalledWith("native_portable_handoff_cleanup", expect.anything());
    expect(intent?.requestId).toBe("request-1");
  });

  it("keeps an unavailable prior attempt when the user defers cleanup", async () => {
    mocks.invoke.mockResolvedValueOnce([]);
    mocks.alertSelect.mockResolvedValueOnce("1");

    await resumePortableExportsAfterBootstrap();

    expect(mocks.store.clear).not.toHaveBeenCalled();
    expect(mocks.resume).not.toHaveBeenCalled();
  });
});
