import { bench, describe } from 'vitest'
import { createParserContextFixture, measureParserContext, prepareParserRows } from '../../benchmarks/streaming/parserContext.svelte'

for (const assets of [1000, 4000, 8000]) {
    console.log(`parser-context ${JSON.stringify(measureParserContext(assets))}`)
    const fixture = createParserContextFixture(assets)
    describe(`${assets} proxied assets, 48 rows`, () => {
        bench('deep clone per row', () => { prepareParserRows(fixture, false) }, { time: 1, iterations: 1, warmupTime: 0, warmupIterations: 0 })
        bench('one detached seed with isolated row proxies', () => { prepareParserRows(fixture, true) }, { time: 1, iterations: 1, warmupTime: 0, warmupIterations: 0 })
    })
}
