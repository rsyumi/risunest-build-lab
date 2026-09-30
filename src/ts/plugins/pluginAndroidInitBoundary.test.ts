import { readFileSync } from 'node:fs'
import { runInNewContext } from 'node:vm'
import { describe, expect, it } from 'vitest'

const source = readFileSync('crates/wry/src/android/main_pipe.rs', 'utf8')

describe('Android main-frame initialization script boundary', () => {
    it('honors the native frame flag and preserves unrestricted scripts', () => {
        expect(source).toContain('if init_script.for_main_frame_only')
        expect(source).toMatch(/else\s*\{\s*init_script\.script\s*\}/)
    })
    it('the emitted guard initializes the main frame without exposing its invoke key in a child', () => {
        const format = source.match(/format!\("(if \(window\.top === window\).*?)", init_script\.script\)/s)![1]
        const script = JSON.parse('"' + format + '"').replace('{{', '{').replace('}}', '}').replace('{}',
            'window.__TAURI_INTERNALS__ = { invokeKey: "synthetic", invoke: () => "main-ok" }; // trailing comment')
        const main: any = {}
        main.top = main
        runInNewContext(script, { window: main })
        expect(main.__TAURI_INTERNALS__.invoke()).toBe('main-ok')
        const child: any = { top: main }
        runInNewContext(script, { window: child })
        expect(child.__TAURI_INTERNALS__).toBeUndefined()
    })
})
