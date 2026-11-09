// Observe real product IPC without importing or replacing generation logic.
export function installCommandWitness(internals) {
    if (!internals || typeof internals.invoke !== 'function') throw new Error('ollama-ui:native-invoke-missing')
    const original = internals.invoke.bind(internals)
    const events = []
    let barrier = null
    let release = null
    let blocked = null
    let active = false
    let order = 0
    const clone = value => value === undefined ? null : JSON.parse(JSON.stringify(value))
    const observed = new Set([
        'native_generation_host_init', 'native_generation_reserve_ollama', 'native_generation_execute',
        'native_generation_cancel', 'native_generation_status', 'native_generation_proof',
        'native_generation_capture_witness', 'native_generation_active',
    ])
    const invoke = async (command, args, options) => {
        if (!active || !observed.has(command)) return original(command, args, options)
        const event = { command, state: 'pending', result: null, invocationOrder: ++order, completionOrder: null }
        events.push(event)
        try {
            const result = await original(command, args, options)
            event.completionOrder = ++order
            event.state = 'resolved'
            // Host input, request settings and witness hashes are deliberately omitted.
            if (command !== 'native_generation_host_init' && command !== 'native_generation_capture_witness') {
                event.result = clone(result)
            }
            const matches = barrier === 'before-execute' && command === 'native_generation_capture_witness'
                || barrier === 'terminal-proof' && command === 'native_generation_proof'
                    && result?.at !== 'issued' && result?.receipts?.output
            if (matches) {
                blocked = barrier
                barrier = null
                await new Promise(resolve => { release = resolve })
                blocked = null
                release = null
            }
            return result
        } catch (error) {
            event.completionOrder ??= ++order
            event.state = 'rejected'
            throw error
        }
    }
    const inspectionCommands = new Set([
        'native_startup_status', 'pds_open', 'pds_replace_begin', 'pds_replace_put_root', 'pds_replace_put_presets',
        'pds_replace_add_characters', 'pds_replace_commit', 'pds_set_device_setting',
        'pds_read_conversation', 'native_generation_status', 'native_generation_proof', 'native_generation_active',
        'ollama_ui_arm', 'ollama_ui_snapshot', 'ollama_ui_release', 'ollama_ui_retire',
    ])
    return Object.freeze({
        invoke,
        inspect(command, args) {
            if (!inspectionCommands.has(command)) throw new Error('ollama-ui:inspection-command-refused')
            return original(command, args)
        },
        begin() {
            if (active || blocked) throw new Error('ollama-ui:witness-active')
            events.length = 0
            active = true
        },
        arm(value) {
            if (!['before-execute', 'terminal-proof'].includes(value) || barrier || blocked) {
                throw new Error('ollama-ui:invalid-barrier')
            }
            barrier = value
        },
        resume() {
            if (!release) throw new Error('ollama-ui:barrier-not-held')
            release()
        },
        snapshot() { return clone({ events, blocked, active }) },
        end() {
            if (blocked || barrier) throw new Error('ollama-ui:unresolved-barrier')
            active = false
            return clone({ events, blocked, active })
        },
    })
}


export function hasStatusAfterIssuedProof(value) {
    return value.events.some(event =>
        event.command === 'native_generation_proof' && event.state === 'resolved' && event.result?.at === 'issued'
        && Number.isSafeInteger(event.completionOrder)
        && value.events.some(next => next.command === 'native_generation_status'
            && (next.state === 'pending' || next.state === 'resolved')
            && Number.isSafeInteger(next.invocationOrder) && next.invocationOrder > event.completionOrder))
}
