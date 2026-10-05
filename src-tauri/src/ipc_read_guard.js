// Tauri's IPC script treats a body read that fails after a custom-protocol
// response as a blocked protocol, and from then on every response returns as
// one evaluated script. Read IPC bodies here so a failed read rejects only its
// own command. Android uses the custom protocol only for channel data, which a
// postMessage resend would deliver as such a script, so a rejected channel
// fetch is handled the same way there. Other targets keep Tauri's fallback for
// a protocol that is blocked before any response.
// A block also separates this from Tauri's preceding, unterminated IIFE.
{
  const android = __RISUNEST_ANDROID__;
  const channelFetchPath = encodeURIComponent("plugin:__TAURI_CHANNEL__|fetch");
  const ipcPrefixes = [
    "ipc://localhost/",
    "http://ipc.localhost/",
    "https://ipc.localhost/",
  ];
  const originalFetch = window.fetch.bind(window);

  const ipcPath = (input) => {
    if (typeof input !== "string") return null;
    const prefix = ipcPrefixes.find((candidate) => input.startsWith(candidate));
    return prefix === undefined ? null : input.slice(prefix.length);
  };

  const failed = (error) => {
    const message = `IPC response could not be read (${String(error)})`;
    return {
      headers: new Headers({
        "Content-Type": "text/plain",
        "Tauri-Response": "error",
      }),
      text: () => Promise.resolve(message),
    };
  };

  // Tauri's script picks the reader the same way; Android may repeat the type.
  const read = (response) => {
    const headers = response.headers;
    switch ((headers.get("content-type") || "").split(",")[0]) {
      case "application/json":
        return response
          .json()
          .then((value) => ({ headers, json: () => Promise.resolve(value) }));
      case "text/plain":
        return response
          .text()
          .then((value) => ({ headers, text: () => Promise.resolve(value) }));
      default:
        return response.arrayBuffer().then((value) => ({
          headers,
          arrayBuffer: () => Promise.resolve(value),
        }));
    }
  };

  window.fetch = (input, init) => {
    const path = ipcPath(input);
    if (path === null) return originalFetch(input, init);
    return originalFetch(input, init).then(
      (response) => read(response).catch(failed),
      (error) => {
        if (android && path === channelFetchPath) return failed(error);
        throw error;
      },
    );
  };
}
