// Times V8 JSON.parse and JSON.stringify on the inputs the `json_parse_cost` Rust measurement writes to JSON_COST_DUMP:
// node benchmarks/rust-core-json/v8JsonCost.mjs <directory>
import { readdirSync, readFileSync } from 'node:fs'
import { join } from 'node:path'

const directory = process.argv[2]
if (!directory) throw new Error('Usage: node benchmarks/rust-core-json/v8JsonCost.mjs <directory>')

const shapes = ['lorebook', 'long-string', 'deep', 'numbers', 'unicode', 'escapes', 'wide']
const kib = label => Number(label.slice(0, -1)) * (label.endsWith('m') ? 1024 : 1)
const inputs = readdirSync(directory).map(name => /^(.+)-(\d+[km])\.json$/.exec(name)).filter(match => match && shapes.includes(match[1]))
    .sort((left, right) => shapes.indexOf(left[1]) - shapes.indexOf(right[1]) || kib(left[2]) - kib(right[2]))

// Median milliseconds over samples sized to run at least 200 ms in total, as the Rust measurement does.
function measure(operation) {
    const once = operation()
    const samples = Math.min(200, Math.max(5, Math.floor(200 / Math.max(once, 0.001))))
    const times = Array.from({ length: samples }, operation).sort((left, right) => left - right)
    return times[Math.floor(samples / 2)]
}

console.log('| input | bytes | parse ms | parse ms/MiB | stringify ms/MiB |')
console.log('| --- | --- | --- | --- | --- |')
for (const [file, shape, size] of inputs) {
    const bytes = readFileSync(join(directory, file))
    const source = bytes.toString('utf8')
    const mib = bytes.length / (1 << 20)
    let value
    const parse = measure(() => {
        const started = performance.now()
        value = JSON.parse(source)
        return performance.now() - started
    })
    let text
    const stringify = measure(() => {
        const started = performance.now()
        text = JSON.stringify(value)
        return performance.now() - started
    })
    if (text.length === 0) throw new Error('Empty stringify result')
    console.log(`| ${shape}-${size} | ${bytes.length} | ${parse.toFixed(1)} | ${(parse / mib).toFixed(2)} | ${(stringify / mib).toFixed(2)} |`)
}
