// @vitest-environment happy-dom
import { afterEach, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'
import { SvelteMap } from 'svelte/reactivity'
import TransferSpeed from './TransferSpeed.svelte'
import type { TransferRateSample } from 'src/ts/storage/sync/transferRate'

let component: ReturnType<typeof mount> | undefined
let target: HTMLDivElement
const samples = new SvelteMap<string, TransferRateSample>()
const sample = (atMs: number, sentBytes = '0', receivedBytes = '0', id = 'request') => ({ id, atMs, sentBytes, receivedBytes, sending: true, receiving: true })
async function show() {
    target = document.createElement('div'); document.body.append(target)
    component = mount(TransferSpeed, { target, props: { get sample() { return samples.get('current')! }, uploadLabel: '업로드 속도', downloadLabel: '다운로드 속도' } })
    await tick()
}
afterEach(async () => { if (component) await unmount(component); component = undefined; target?.remove(); samples.clear(); vi.useRealTimers() })

it('shows independent body rates and reaches zero when an active stream stalls', async () => {
    vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval', 'performance'] })
    samples.set('current', sample(0)); await show()
    expect(target.textContent).toBe('')
    samples.set('current', sample(1000, '2097152', '1048576')); await tick()
    expect(target.textContent).toContain('↑ 2.0 MiB/s')
    expect(target.textContent).toContain('↓ 1.0 MiB/s')
    await vi.advanceTimersByTimeAsync(3500); await tick()
    expect(target.textContent).toContain('↑ 0 B/s')
    expect(target.textContent).toContain('↓ 0 B/s')
})

it('hides a stopped direction and resets its samples when the native operation changes', async () => {
    vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval', 'performance'] })
    samples.set('current', sample(0)); await show()
    samples.set('current', sample(1000, '2097152', '1048576')); await tick()
    samples.set('current', { ...sample(1500, '2097152', '1572864'), sending: false }); await tick()
    expect(target.textContent).not.toContain('↑')
    expect(target.textContent).toContain('↓ 1.0 MiB/s')
    samples.set('current', sample(0, '0', '0', 'next')); await tick()
    expect(target.textContent).toBe('')
    samples.set('current', { ...sample(1000, '0', '1024', 'next'), sending: false }); await tick()
    expect(target.textContent).toContain('↓ 1.0 KiB/s')
    samples.set('current', { ...sample(1500, '0', '1024', 'next'), sending: false, receiving: false }); await tick()
    expect(target.textContent).toBe('')
})
