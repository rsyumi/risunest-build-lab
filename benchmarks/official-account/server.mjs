import { createServer } from 'node:http'

export async function startOfficialAccountFixtureServer() {
    const server = createServer((request, response) => {
        response.setHeader('Access-Control-Allow-Origin', '*')
        response.setHeader('Access-Control-Allow-Headers', 'x-risu-key')
        if (request.method === 'OPTIONS') { response.writeHead(204).end(); return }
        const path = request.url?.match(/^\/api\/account\/read\/([a-f0-9]+)$/)?.[1]
        const key = path ? Buffer.from(path, 'hex').toString('utf8') : ''
        const fixture = key.match(/^assets\/official-measure-(\d+)-(\d+)-(\d+)\.bin$/)
        if (request.method !== 'GET' || !fixture || request.headers['x-risu-key'] !== key) {
            response.writeHead(400).end(); return
        }
        const [, seedText, latencyText, sizeText] = fixture
        const seed = Number(seedText), latency = Number(latencyText), size = Number(sizeText)
        if (latency > 1000 || size > 8 * 1024 * 1024) { response.writeHead(400).end(); return }
        const timer = setTimeout(() => {
            const bytes = Buffer.alloc(size, seed % 251)
            if (size >= 4) bytes.writeUInt32LE(seed, 0)
            response.writeHead(200, { 'Content-Type': 'application/octet-stream', 'Content-Length': size }).end(bytes)
        }, latency)
        response.on('close', () => clearTimeout(timer))
    })
    await new Promise((resolve, reject) => {
        server.once('error', reject)
        server.listen(0, '127.0.0.1', resolve)
    })
    return {
        origin: `http://127.0.0.1:${server.address().port}`,
        close: () => new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve())),
    }
}
