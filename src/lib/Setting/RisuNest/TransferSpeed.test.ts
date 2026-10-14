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
const icons = () => [...target.querySelectorAll('[data-transfer-speed] svg')].map(icon => icon.getAttribute('class'))

it('shows independent body rates and reaches zero when an active stream stalls', async () => {
    vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval', 'performance'] })
    samples.set('current', sample(0)); await show()
    expect(target.textContent).toBe('')
    samples.set('current', sample(1000, '2097152', '1048576')); await tick()
    expect(target.textContent).toContain('업로드 속도 2.0 MiB/s')
    expect(target.textContent).toContain('다운로드 속도 1.0 MiB/s')
    expect(icons()).toEqual([expect.stringContaining('lucide-arrow-up-from-line'), expect.stringContaining('lucide-arrow-down-to-line')])
    expect([...target.querySelectorAll('.sr-only')].map(label => label.textContent)).toEqual(['업로드 속도', '다운로드 속도'])
    await vi.advanceTimersByTimeAsync(3500); await tick()
    expect(target.textContent).toContain('업로드 속도 0 B/s')
    expect(target.textContent).toContain('다운로드 속도 0 B/s')
})

it('keeps a stopped direction until its average reaches zero and resets its samples when the native operation changes', async () => {
    vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval', 'performance'] })
    samples.set('current', sample(0)); await show()
    samples.set('current', sample(1000, '2097152', '1048576')); await tick()
    samples.set('current', { ...sample(1500, '2097152', '1572864'), sending: false }); await tick()
    expect(target.textContent).toContain('업로드 속도 1.3 MiB/s')
    expect(target.textContent).toContain('다운로드 속도 1.0 MiB/s')
    await vi.advanceTimersByTimeAsync(3500); await tick()
    expect(target.textContent).not.toContain('업로드 속도')
    expect(target.textContent).toContain('다운로드 속도 0 B/s')
    samples.set('current', sample(0, '0', '0', 'next')); await tick()
    expect(target.textContent).toBe('')
    samples.set('current', { ...sample(1000, '0', '1024', 'next'), sending: false }); await tick()
    expect(target.textContent).toContain('다운로드 속도 1.0 KiB/s')
    expect(target.textContent).not.toContain('업로드 속도')
    samples.set('current', { ...sample(1500, '0', '1024', 'next'), sending: false, receiving: false }); await tick()
    expect(target.textContent).toContain('다운로드 속도 683 B/s')
    await vi.advanceTimersByTimeAsync(3500); await tick()
    expect(target.textContent).toBe('')
})

it('stays visible between request bodies that leave in short bursts', async () => {
    vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval', 'performance'] })
    const burst = (atMs: number, sentBytes: number, sending: boolean) => ({ ...sample(atMs, String(sentBytes)), sending, receiving: false })
    samples.set('current', burst(0, 0, false)); await show()
    for (let second = 1; second <= 6; second++) {
        samples.set('current', burst(second * 1000, (second - 0.5) * 1048576, true)); await tick()
        expect(target.textContent).toContain('업로드 속도')
        samples.set('current', burst(second * 1000 + 100, second * 1048576, false)); await tick()
        for (let wait = 0; wait < 900; wait += 300) {
            await vi.advanceTimersByTimeAsync(300); await tick()
            expect(target.textContent).toContain('업로드 속도')
        }
    }
    expect(target.textContent).toMatch(/업로드 속도 \d+(\.\d)? (KiB|MiB)\/s/)
    await vi.advanceTimersByTimeAsync(3500); await tick()
    expect(target.textContent).toBe('')
})
