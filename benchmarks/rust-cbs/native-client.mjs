import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';

export function nativeClient(binary, args = []) {
  const child = spawn(binary, args, { stdio: ['pipe', 'pipe', 'ignore'], windowsHide: true });
  const lines = createInterface({ input: child.stdout });
  let pending;
  let terminal;

  function settle(outcome) {
    if (!pending) return;
    const request = pending;
    pending = undefined;
    clearTimeout(request.timer);
    if (outcome.error) request.reject(outcome.error);
    else request.resolve(outcome.value);
  }

  function stop(outcome) {
    if (terminal) return;
    terminal = outcome;
    settle(outcome);
    lines.close();
    child.stdin.destroy();
    child.kill();
  }

  const fail = error => stop({ error });
  child.on('error', fail);
  child.stdin.on('error', fail);
  child.stdout.on('error', fail);
  lines.on('error', fail);
  child.on('close', () => stop({ value: { error: 'process_exit' } }));
  lines.on('line', line => {
    if (!pending || terminal) return;
    let value;
    try { value = JSON.parse(line); }
    catch (error) { fail(error); return; }
    settle({ value });
  });

  return {
    async send(request, timeout = 3000) {
      if (terminal?.error) throw terminal.error;
      if (terminal) return terminal.value;
      if (pending) throw Error('Concurrent native request');
      const line = JSON.stringify(request) + '\n';
      return new Promise((resolve, reject) => {
        pending = { resolve, reject, timer: setTimeout(() => stop({ value: { error: 'timeout' } }), timeout) };
        try { child.stdin.write(line, error => { if (error) fail(error); }); }
        catch (error) { fail(error); }
      });
    },
    close() { stop({ value: { error: 'process_exit' } }); },
  };
}
