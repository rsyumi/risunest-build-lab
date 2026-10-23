import { describe, expect, it } from 'vitest'
import { galleryWindow } from './galleryWindow'

describe('gallery viewport', () => {
    it('keeps mounted items bounded through a 100,000-item library and reaches the last item', () => {
        for (const mobile of [false,true]) for (const large of [false,true]) for (const captions of [false,true]) {
            const input={total:100_000,width:mobile?343:1100,viewportHeight:740,scrollOffset:0,large,mobile,captions}
            const first=galleryWindow(input)
            let peak=0
            for (let position=0;position<first.height;position+=first.height/1000) {
                const current=galleryWindow({...input,scrollOffset:position})
                peak=Math.max(peak,current.limit)
                expect(current.offset).toBeGreaterThanOrEqual(0)
                expect(current.offset+current.limit).toBeLessThanOrEqual(input.total)
                const visibleItem=Math.min(input.total-1,Math.floor(position/current.rowHeight)*current.columns)
                expect(visibleItem).toBeGreaterThanOrEqual(current.offset)
                expect(visibleItem).toBeLessThan(current.offset+current.limit)
            }
            expect(peak).toBeLessThan(100)
            const end=galleryWindow({...input,scrollOffset:first.height-input.viewportHeight})
            expect(end.offset+end.limit).toBe(input.total)
            expect(end.height).toBe(first.height)
        }
    })
    it('uses three phone columns normally and two large columns, with fixed square cells', () => {
        const input={total:17,width:343,viewportHeight:700,scrollOffset:0,large:false,mobile:true,captions:false}
        const normal=galleryWindow(input)
        const large=galleryWindow({...input,large:true})
        expect(normal.columns).toBe(3)
        expect(large.columns).toBe(2)
        expect(normal.cell).toBe(105)
        expect(large.cell).toBe(160.5)
        expect(galleryWindow({...input,captions:true}).cell).toBe(normal.cell)
        expect(galleryWindow({...input,total:0}).limit).toBe(0)
    })
})
