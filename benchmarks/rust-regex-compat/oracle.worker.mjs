import { parentPort } from 'node:worker_threads'
import { text, units } from './fixtures.mjs'
parentPort.on('message', ({ rules, input, probe, controlledProbe, start = 0 }) => {
    try {
        const compiled = rules.map(rule => ({ regex: new RegExp(text(rule.pattern), controlledProbe && !rule.flags.includes('d') ? rule.flags + 'd' : rule.flags), replacement: text(rule.replacement) }))
        if (controlledProbe) {
            compiled[0].regex.lastIndex = start
            const found = compiled[0].regex.exec(text(input))
            parentPort.postMessage({ status: 'controlledSuccess', range: found ? found.indices[0] : null,
                captures: found ? found.indices.slice(1).map(range => range ?? null) : null,
                namedCaptures: found?.indices.groups ? Object.fromEntries(Object.entries(found.indices.groups).map(([name, range]) => [name, range ?? null])) : {} })
            return
        }
        if (probe) {
            const found = compiled[0].regex.exec(text(input))
            parentPort.postMessage({ status: 'probeSuccess', range: found ? [found.index, found.index + found[0].length] : null })
            return
        }
        let output = text(input)
        for (const rule of compiled) { rule.regex.lastIndex = 0; output = output.replace(rule.regex, rule.replacement) }
        parentPort.postMessage({ status: 'jsSuccess', output: units(output) })
    } catch (error) {
        parentPort.postMessage({ status: 'jsRejected', name: error.name })
    }
})
parentPort.postMessage({ status: 'ready' })
