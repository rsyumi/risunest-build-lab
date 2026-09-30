import { expect, it } from 'vitest'
import { normalizeDownloadFileName } from './downloadFileName'
it.each([['a:b/c\\d?*.json','a_b_c_d__.json'], ['...','export'], ['','export'], ['safe.json. ','safe.json'], ['CON.txt','_CON.txt'], ['x\u0000y.json','x_y.json']])('normalizes %j into a portable basename', (name, expected) => {
    expect(normalizeDownloadFileName(name)).toBe(expected)
})
