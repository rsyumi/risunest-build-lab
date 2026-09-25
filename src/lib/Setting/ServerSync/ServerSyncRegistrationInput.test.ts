// @vitest-environment happy-dom
import { afterEach, describe, expect, it, vi } from "vitest";
import { mount, tick, unmount } from "svelte";
vi.mock("src/ts/platform", () => ({ isTauriAndroid: false, isTauriIOS: true }));
import Input from "./ServerSyncRegistrationInput.svelte";
import { serverRegistrationInbox } from "src/ts/storage/sync/serverSyncRegistrationInbox";
import { MAX_REGISTRATION_URI_BYTES } from "src/ts/storage/sync/serverSyncRegistration";
import vector from "../../../../crates/sync-connect/tests/registration-vector.json";
let component: ReturnType<typeof mount> | undefined;
afterEach(async () => {
  if (component) await unmount(component);
  component = undefined;
  serverRegistrationInbox.clear();
  document.body.replaceChildren();
});
describe("registration form input", () => {
  it("offers the existing scanner on iOS", async () => {
    const target = document.createElement("div");
    document.body.append(target);
    component = mount(Input, { target, props: { onRegistration: vi.fn() } });
    await tick();
    expect(target.querySelectorAll("button")).toHaveLength(2);
  });
  it("keeps the registration code masked, unchanged by the keyboard and bounded by the URI limit", async () => {
    const target = document.createElement("div");
    document.body.append(target);
    component = mount(Input, { target, props: { onRegistration: vi.fn() } });
    await tick();
    const input = target.querySelector("input")!;
    expect(input.type).toBe("password");
    expect(input.getAttribute("autocomplete")).toBe("new-password");
    expect(input.getAttribute("autocapitalize")).toBe("none");
    expect(input.getAttribute("spellcheck")).toBe("false");
    expect(input.getAttribute("maxlength")).toBe(
      String(MAX_REGISTRATION_URI_BYTES),
    );
    expect(vector.uri.length).toBeLessThanOrEqual(MAX_REGISTRATION_URI_BYTES);
  });
  it("prefills only through explicit Read code and clears the raw secret", async () => {
    const target = document.createElement("div");
    document.body.append(target);
    const onRegistration = vi.fn();
    component = mount(Input, { target, props: { onRegistration } });
    await tick();
    const input = target.querySelector("input")!;
    input.value = vector.uri;
    input.dispatchEvent(new Event("input", { bubbles: true }));
    await tick();
    expect(onRegistration).not.toHaveBeenCalled();
    target.querySelector("button")!.click();
    await tick();
    expect(onRegistration).toHaveBeenCalledWith(vector.registration);
    expect(input.value).toBe("");
    expect(target.textContent).not.toContain(vector.registration.token);
  });
  it("rejects OS delivery while connected and allows the same link to be delivered again", async () => {
    serverRegistrationInbox.stage(vector.uri);
    const target = document.createElement("div");
    document.body.append(target);
    const onRegistration = vi.fn();
    component = mount(Input, {
      target,
      props: { available: false, onRegistration },
    });
    await tick();
    expect(onRegistration).not.toHaveBeenCalled();
    await vi.waitFor(() =>
      expect(target.querySelector("[role=status]")).not.toBeNull(),
    );
    expect(serverRegistrationInbox.take()).toBeUndefined();
    expect(serverRegistrationInbox.stage(vector.uri)).toBe(true);
  });
});
