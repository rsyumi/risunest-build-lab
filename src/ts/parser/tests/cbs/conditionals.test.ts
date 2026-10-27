import { compileDisplayCbs, createDisplayCbsPlan, DisplayCbsExecutor, projectDisplayCbsContext } from '../../../process/displayCbsCompiler'
import fc from 'fast-check'
import { writable } from 'svelte/store'
import { afterEach, describe, expect, test, vi } from 'vitest'
import { risuChatParser } from '../../parser.svelte'
import { trimVarPrefix, validCBSArgProp } from './lib'

//#region module mocks

vi.mock(
  import('../../../storage/database.svelte'),
  () =>
    ({
      appVer: '1234.5.67',
      getCurrentCharacter: () => ({}),
      getCurrentChat: () => ({}),
      getDatabase: () => ({}),
    }) as typeof import('../../../storage/database.svelte'),
)

vi.mock(import('../../../globalApi.svelte'), () => ({
  aiWatermarkingLawApplies: () => false,
  getFileSrc: () => Promise.resolve(''),
}))

/** Returns accessed key as the value. */
const varStorage = vi.hoisted(
  () =>
    new Proxy(
      {},
      {
        get(_, prop) {
          return trimVarPrefix(prop)
        },
      },
    ),
)

vi.mock(import('../../../stores.svelte'), () => {
  return {
    DBState: {
      db: {
        characters: [
          {
            chatPage: 0,
            chats: [
              {
                scriptstate: varStorage,
              },
            ],
            defaultVariables: '',
          },
        ],
        globalChatVariables: varStorage,
        templateDefaultVariables: '',
      },
    },
    selIdState: {
      selId: 0,
    },
    selectedCharID: writable(0),
  } as typeof import('../../../stores.svelte')
})

//#endregion

const template = (op: string, body: string) => `0 {{${op}}}${body}{{/}} 9`
const quickParse = (...args: Parameters<typeof template>) => risuChatParser(template(...args))

const indentedBody = `

  C  
  B  
  S  

`

afterEach(() => {
  vi.resetAllMocks()
})

describe('#if', () => {
  test('renders when "1" or "true"', () => {
    expect(quickParse('#if 1', 'CBS')).toBe(`0 CBS 9`)
    expect(quickParse('#if true', 'CBS')).toBe(`0 CBS 9`)

    // Edge case: {{#if 1\s+.*}} also renders
    fc.assert(
      fc.property(fc.constantFrom('1', 'true'), fc.stringMatching(/^ +[^#:{}\r\n]*$/), (truthy, tail) => {
        expect(quickParse(`#if ${truthy}${tail}`, 'CBS')).toBe(`0 CBS 9`)
      }),
    )
  })

  test('does not render when anything else', async () => {
    expect(quickParse('#if 0', 'CBS')).toBe('0  9')
    expect(quickParse('#if false', 'CBS')).toBe('0  9')

    // Edge case: {{#if \s+1}} does not render
    expect(quickParse('#if  1', 'CBS')).toBe(`0  9`)
    expect(quickParse('#if   true', 'CBS')).toBe(`0  9`)

    fc.assert(
      fc.property(
        validCBSArgProp.filter((s) => !/^1|(?:true)\s*/.test(s)),
        (anythingElse) => {
          expect(quickParse(`#if ${anythingElse}`, 'CBS')).toBe(`0  9`)
        },
      ),
    )
  })

  test('trims start of the block, end of the block, and start of each line', () => {
    expect(quickParse('#if 1', indentedBody)).toBe('0 C  \nB  \nS 9')
  })

  test('can be nested', () => {
    expect(quickParse('#if 1', template('#if 1', 'CBS'))).toBe(`0 0 CBS 9 9`)
    expect(quickParse('#if 1', template('#if 0', 'CBS'))).toBe(`0 0  9 9`)
    expect(quickParse('#if 0', template('#if 1', 'CBS'))).toBe(`0  9`)
    expect(quickParse('#if 0', template('#if 0', 'CBS'))).toBe(`0  9`)
  })
})

describe('legacy {#if}', () => {
  test('renders the body after its newline when truthy', () => {
    expect(risuChatParser('a {#if 1\n shown #} b')).toBe('a shown b')
    expect(risuChatParser('a {#if 0\nhidden#} b')).toBe('a  b')
    expect(risuChatParser('a{#if 1\n{{#if 1}}x{{/}}#}b')).toBe('axb')
  })

  test('keeps braces and hashes that open or close nothing', () => {
    expect(risuChatParser('a{b}c#d}#{e #')).toBe('a{b}c#d}#{e #')
    expect(risuChatParser('{#if 1#} x {#y#}')).toBe('{#if 1#} x {#y#}')
  })
})

describe('#if_pure', () => {
  test('renders when "1" or "true"', () => {
    expect(quickParse('#if_pure 1', 'CBS')).toBe(`0 CBS 9`)
    expect(quickParse('#if_pure true', 'CBS')).toBe(`0 CBS 9`)

    // Edge case: {{#if_pure 1\s+.*}} also renders
    fc.assert(
      fc.property(fc.constantFrom('1', 'true'), fc.stringMatching(/^ +[^#:{}\r\n]*$/), (truthy, tail) => {
        expect(quickParse(`#if_pure ${truthy}${tail}`, 'CBS')).toBe(`0 CBS 9`)
      }),
    )
  })

  test('does not render when anything else', async () => {
    expect(quickParse('#if_pure 0', 'CBS')).toBe('0  9')
    expect(quickParse('#if_pure false', 'CBS')).toBe('0  9')

    // Edge case: {{#if_pure \s+1}} does not render
    expect(quickParse('#if_pure  1', 'CBS')).toBe(`0  9`)
    expect(quickParse('#if_pure   true', 'CBS')).toBe(`0  9`)

    fc.assert(
      fc.property(
        validCBSArgProp.filter((s) => !/^1|(?:true)\s*/.test(s)),
        (anythingElse) => {
          expect(quickParse(`#if_pure ${anythingElse}`, 'CBS')).toBe(`0  9`)
        },
      ),
    )
  })

  test('preserves all whitespaces', () => {
    expect(quickParse('#if_pure 1', indentedBody)).toBe(`0 ${indentedBody} 9`)
  })

  test('can be nested', () => {
    expect(quickParse('#if_pure 1', template('#if_pure 1', indentedBody))).toBe(`0 0 ${indentedBody} 9 9`)
    expect(quickParse('#if_pure 1', template('#if_pure 0', indentedBody))).toBe(`0 0  9 9`)
    expect(quickParse('#if_pure 0', template('#if_pure 1', indentedBody))).toBe(`0  9`)
    expect(quickParse('#if_pure 0', template('#if_pure 0', indentedBody))).toBe(`0  9`)
  })
})

describe('#when', () => {
  test('renders when "1" or "true"', () => {
    expect(quickParse('#when::1', 'CBS')).toBe(`0 CBS 9`)
    expect(quickParse('#when::true', 'CBS')).toBe(`0 CBS 9`)
  })

  test('does not render when anything else', async () => {
    expect(quickParse('#when::0', 'CBS')).toBe('0  9')
    expect(quickParse('#when::false', 'CBS')).toBe('0  9')

    fc.assert(
      fc.property(
        validCBSArgProp.filter((s) => s !== '1' && s !== 'true'),
        (anythingElse) => {
          expect(quickParse(`#when::${anythingElse}`, 'CBS')).toBe(`0  9`)
        },
      ),
    )
  })

  test('removes line breaks at block start/end, preserves all other whitespaces', () => {
    expect(quickParse('#when::1', indentedBody)).toBe(`0 ${indentedBody.replaceAll(/(^\n+|\n+$)/g, '')} 9`)
  })

  test('can be nested', () => {
    expect(quickParse('#when 1', template('#when 1', 'CBS'))).toBe(`0 0 CBS 9 9`)
    expect(quickParse('#when 1', template('#when 0', 'CBS'))).toBe(`0 0  9 9`)
    expect(quickParse('#when 0', template('#when 1', 'CBS'))).toBe(`0  9`)
    expect(quickParse('#when 0', template('#when 0', 'CBS'))).toBe(`0  9`)
  })

  test('can omit :: without operators', () => {
    expect(quickParse('#when 1', 'CBS')).toBe(`0 CBS 9`)
    expect(quickParse('#when 0', 'CBS')).toBe('0  9')
  })

  describe('Operators: equality/inequality', () => {
    test('::is, isnot', () => {
      fc.assert(
        fc.property(validCBSArgProp, validCBSArgProp, (a, b) => {
          fc.pre(a !== b)

          expect(quickParse(`#when::${a}::is::${a}`, 'CBS')).toBe('0 CBS 9')
          expect(quickParse(`#when::${a}::is::${b}`, 'CBS')).toBe('0  9')

          expect(quickParse(`#when::${a}::isnot::${a}`, 'CBS')).toBe('0  9')
          expect(quickParse(`#when::${a}::isnot::${b}`, 'CBS')).toBe('0 CBS 9')
        }),
      )
    })

    test('::>, >=, <=, <', () => {
      fc.assert(
        fc.property(fc.integer(), fc.integer(), (a, b) => {
          fc.pre(a > b)

          expect(quickParse(`#when::${a}::>::${a}`, 'CBS')).toBe('0  9')
          expect(quickParse(`#when::${a}::>::${b}`, 'CBS')).toBe('0 CBS 9')
          expect(quickParse(`#when::${b}::>::${a}`, 'CBS')).toBe('0  9')

          expect(quickParse(`#when::${a}::>=::${a}`, 'CBS')).toBe('0 CBS 9')
          expect(quickParse(`#when::${a}::>=::${b}`, 'CBS')).toBe('0 CBS 9')
          expect(quickParse(`#when::${b}::>=::${a}`, 'CBS')).toBe('0  9')

          expect(quickParse(`#when::${a}::<=::${a}`, 'CBS')).toBe('0 CBS 9')
          expect(quickParse(`#when::${a}::<=::${b}`, 'CBS')).toBe('0  9')
          expect(quickParse(`#when::${b}::<=::${a}`, 'CBS')).toBe('0 CBS 9')

          expect(quickParse(`#when::${a}::<::${a}`, 'CBS')).toBe('0  9')
          expect(quickParse(`#when::${a}::<::${b}`, 'CBS')).toBe('0  9')
          expect(quickParse(`#when::${b}::<::${a}`, 'CBS')).toBe('0 CBS 9')
        }),
      )
    })
  })

  describe('Operators: logical', () => {
    test('::and', () => {
      expect(quickParse('#when::1::and::1', 'CBS')).toBe(`0 CBS 9`)
      expect(quickParse('#when::1::and::not::true', 'CBS')).toBe(`0  9`)

      expect(quickParse('#when::1::and::0', 'CBS')).toBe(`0  9`)
      expect(quickParse('#when::1::and::not::false', 'CBS')).toBe(`0 CBS 9`)
    })

    test('::or', () => {
      expect(quickParse('#when::0::or::1', 'CBS')).toBe(`0 CBS 9`)
      expect(quickParse('#when::0::or::not::true', 'CBS')).toBe(`0  9`)

      expect(quickParse('#when::0::or::0', 'CBS')).toBe(`0  9`)
      expect(quickParse('#when::0::or::not::false', 'CBS')).toBe(`0 CBS 9`)
    })

    test('right-to-left evaluation', () => {
      expect(quickParse('#when::1::or::0::and::0', 'CBS')).toBe(`0 CBS 9`)
      expect(quickParse('#when::0::or::1::and::0', 'CBS')).toBe(`0  9`)
      expect(quickParse('#when::0::or::0::and::1', 'CBS')).toBe(`0  9`)

      expect(quickParse('#when::1::and::1::or::0', 'CBS')).toBe(`0 CBS 9`)
      expect(quickParse('#when::1::and::0::or::1', 'CBS')).toBe(`0 CBS 9`)
      expect(quickParse('#when::0::and::1::or::1', 'CBS')).toBe(`0  9`)
    })

    test.skip('Lower precedence than other operators', () => {
      // FIXME: left-hand/right-hand must be evaluated first, then or
      // Given #when::a::tis::3::or::b::tis::7
      //   AS-IS: a::tis::3 -> 1, 1::or::7 -> 1, 1::tis::7 -> 0
      //   TO-BE: a::tis::3 -> 1, b::tis::7 -> 1, 1::or::1 -> 1
      expect(quickParse('#when::3::tis::3::or::7::tis::7', 'CBS')).toBe(`0 CBS 9`)
    })
  })

  describe('Operators: whitespaces', () => {
    test('::keep preserves all whitespaces', () => {
      expect(quickParse('#when::keep::1', indentedBody)).toBe(`0 ${indentedBody} 9`)
    })

    test('::legacy trims start of the block, end of the block, and start of each line', () => {
      expect(quickParse('#when::legacy::1', indentedBody)).toBe(`0 C  \nB  \nS 9`)
    })
  })

  describe('Operators: variables', () => {
    test('::var, toggle', () => {
      expect(quickParse('#when::var::true', 'CBS')).toBe(`0 CBS 9`)
      expect(quickParse('#when::toggle::true', 'CBS')).toBe(`0 CBS 9`)

      fc.assert(
        fc.property(
          validCBSArgProp.filter((s) => s !== '1' && s !== 'true'),
          (anythingElse) => {
            expect(quickParse(`#when::var::${anythingElse}`, 'CBS')).toBe(`0  9`)
            expect(quickParse(`#when::toggle::${anythingElse}`, 'CBS')).toBe(`0  9`)
          },
        ),
      )
    })

    test('::vis, visnot, tis, tisnot', () => {
      fc.assert(
        fc.property(validCBSArgProp, validCBSArgProp, (a, b) => {
          fc.pre(a !== b)

          expect(quickParse(`#when::${a}::vis::${a}`, 'CBS')).toBe(`0 CBS 9`)
          expect(quickParse(`#when::${a}::vis::${b}`, 'CBS')).toBe(`0  9`)
          expect(quickParse(`#when::${a}::tis::${a}`, 'CBS')).toBe(`0 CBS 9`)
          expect(quickParse(`#when::${a}::tis::${b}`, 'CBS')).toBe(`0  9`)

          expect(quickParse(`#when::${a}::visnot::${a}`, 'CBS')).toBe(`0  9`)
          expect(quickParse(`#when::${a}::visnot::${b}`, 'CBS')).toBe(`0 CBS 9`)
          expect(quickParse(`#when::${a}::tisnot::${a}`, 'CBS')).toBe(`0  9`)
          expect(quickParse(`#when::${a}::tisnot::${b}`, 'CBS')).toBe(`0 CBS 9`)
        }),
      )
    })
  })

  describe('else', () => {
    test('single line else', () => {
      expect(quickParse('#when::1', 'CBS{{:else}}SBC')).toBe(`0 CBS 9`)
      expect(quickParse('#when::0', 'CBS{{:else}}SBC')).toBe(`0 SBC 9`)
    })

    test('multiline else', () => {
      expect(quickParse('#when::1', 'CBS\n{{:else}}\nSBC')).toBe(`0 CBS 9`)
      expect(quickParse('#when::0', 'CBS\n{{:else}}\nSBC')).toBe(`0 SBC 9`)
    })

    test('with ::keep', () => {
      const revBody = [...indentedBody].reverse().join('')

      // FIXME: Unexpected line break removal before the {{:else}}
      expect(quickParse('#when::keep::1', `${indentedBody}{{:else}}${revBody}`)).toBe(
        `0 ${indentedBody.replace(/\n$/, '')} 9`,
      )
      // FIXME: Unexpected line break removal after the {{:else}}
      expect(quickParse('#when::keep::0', `${indentedBody}{{:else}}${revBody}`)).toBe(
        `0 ${revBody.replace(/^\n/, '')} 9`,
      )
    })

    test('works in and out of a nested #when', () => {
      const nestedTemplate = (a: string, b: string) => `{{#when ${a}}}
{{#when ${b}}}CBS{{:else}}SBC{{/}}
{{:else}}
ABC
{{/}}`

      expect(risuChatParser(nestedTemplate('1', '1'))).toBe(`CBS`)
      expect(risuChatParser(nestedTemplate('1', '0'))).toBe(`SBC`)
      expect(risuChatParser(nestedTemplate('0', '1'))).toBe(`ABC`)
      expect(risuChatParser(nestedTemplate('0', '0'))).toBe(`ABC`)
    })

    test('works in an #each', () => {
      const template = `{{#each [1, 2, 3] as n}}
{{#when::n::is::2}}
CBS{{slot::n}}
{{:else}}
SBC{{slot::n}}
{{/}}
{{/}}`
      expect(risuChatParser(template)).toBe(`SBC1SBC2SBC3`)
    })
  })
})

describe('compiled display CBS spans', () => {
  const evaluate = (source: string, variables: Record<string, string> = {}) => {
    const plan = createDisplayCbsPlan([source])
    expect(plan).toBeDefined()
    return new DisplayCbsExecutor(plan!).evaluate(source, { variables })
  }

  test('preserves dynamic separators, JS coercion and conditional whitespace', () => {
    const templates = [
      '{{equal::{{getvar::value}}::a}}',
      '{{equal:{{getvar:value}}:a}}',
      '{{greater::{{getvar::value}}::2}}|{{not::{{getvar::value}}}}',
      '{{#if {{equal::{{getvar::value}}::1}}}} \n  first \n    second \n {{/if}}',
      '{{#if_pure {{getvar::value}}}} \n  unchanged \n {{/if}}',
      '{{#when::{{getvar::value}}}}yes{{:else}}no{{/when}}',
      '{{#when {{getvar::value}}}}\n  yes\n{{:else}}\n  no\n{{/}}',
      '{{#if 1}}{{#when 0}}a{{:else}}b{{/}}{{/}}',
    ]
    for (const source of templates) {
      for (const value of ['', '1', 'true', '0', 'a::a', 'a:a', ' 3 ', 'NaN', '{{random::a::b}}', 'a\nb']) {
        const variables = { value }
        const result = evaluate(source, variables)
        if (result) expect(result.data).toBe(risuChatParser(source, { chatVariables: variables }))
      }
    }
    expect(evaluate(templates[0], { value: 'a::a' })?.data).toBe('1')
    expect(evaluate(templates[1], { value: 'a:a' })?.data).toBe('1')
  })

  test('executes pure string, array, button and arithmetic callbacks with original JS results', () => {
    const cases: [string, string][] = [
      ['{{button::<b>Go</b>::a"b}}', '<button class="button-default" risu-trigger="a"b"><b>Go</b></button>'],
      ['{{button}}', '<button class="button-default" risu-trigger="undefined">undefined</button>'],
      ['{{startswith::abc::a}}', '1'], ['{{endswith::abc::c}}', '1'], ['{{contains::abc::b}}', '1'],
      ['{{replace::aba::a::x}}', 'xbx'], ['{{split::a,b::,}}', '["a","b"]'],
      ['{{join::[1,null,{"a":2},[3,4]]::|}}', '1||[object Object]|3,4'],
      ['{{spread::a§b}}', 'a::b'], ['{{trim::  x  }}', 'x'], ['{{length::😀}}', '2'],
      ['{{arraylength::null}}', '1'], ['{{lower::ABC}}', 'abc'], ['{{upper::abc}}', 'ABC'],
      ['{{capitalize::ßeta}}', 'SSeta'], ['{{round::-1.5}}', '-1'], ['{{floor::-1.5}}', '-2'],
      ['{{ceil::-1.5}}', '-1'], ['{{abs::-2}}', '2'], ['{{remaind::-5::2}}', '-1'],
      ['{{tonumber::a 1.2z}}', ' 1.2'], ['{{pow::2::3}}', '8'], ['{{round}}', 'NaN'],
      ['{{arrayelement::[1,{"a":2},null]::-2}}', '{"a":2}'],
      ['{{arrayelement::[1,null]::1}}', 'null'], ['{{calc::2^3^2}}', '64'], ['{{? (2+3)*4}}', '20'],
    ]
    for (const [source, expected] of cases) {
      expect(evaluate(source), source).toEqual({ data: expected, reads: [] })
      expect(risuChatParser(source), source).toBe(expected)
    }
    const dynamic = '{{replace::{{spread::["a","a","b"]}}}}'
    expect(evaluate(dynamic)).toEqual({ data: 'b', reads: [] })
    expect(evaluate('{{ S_P-L I T ::a,b::,}}')?.data).toBe('["a","b"]')
    expect(evaluate('{{split:{{getvar:value}}:,}}', { value: 'a::b,c' })?.data)
      .toBe(risuChatParser('{{split:{{getvar:value}}:,}}', { chatVariables: { value: 'a::b,c' } }))
  })

  test('preserves array constructor aliases, remaining arrays and missing insertion values', () => {
    const cases: [string, string][] = [
      ['{{makearray}}', '[]'], ['{{makearray::}}', '[""]'],
      ['{{array:a:b}}', '["a","b"]'], ['{{a::1::2}}', '["1","2"]'],
      ['{{ARRAY_POP::[1,2,3]}}', '[1,2]'], ['{{arrayshift::[1,2,3]}}', '[2,3]'],
      ['{{arraypush::[1,2]::x}}', '[1,2,"x"]'], ['{{arraypush::[]}}', '[null]'],
      ['{{arraysplice::[1,2]}}', '[null,1,2]'], ['{{arraysplice::[1,2]::0::1}}', '[null,2]'],
      ['{{arraysplice::[1,2]::Infinity::Infinity::x}}', '[1,2,"x"]'],
      ['{{arraysplice::[1,2]::-Infinity::Infinity::x}}', '["x"]'],
      ['{{arraysplice::[1,2]::-1::NaN::x}}', '[1,"x",2]'],
      ['{{arraysplice::[1,2]::1e100::1e100::x}}', '[1,2,"x"]'],
      ['{{arraypush::1§2}}', '["1","2",null]'],
      ['{{arraypop::{{array::a::b::c}}}}', '["a","b"]'],
      ['{{makearray::{{spread::["a","b"]}}}}', '["a","b"]'],
    ]
    for (const [text, expected] of cases) {
      expect(evaluate(text), text).toEqual({ data: expected, reads: [] })
      expect(risuChatParser(text), text).toBe(expected)
    }
    for (const name of ['arrayshift', 'arraypop', 'arraypush', 'arraysplice']) {
      const text = '{{' + name + '}}'
      expect(evaluate(text), text).toBeNull()
      expect(risuChatParser(text), text).toBe(text)
    }
  })

  test('preserves evaluated array delimiters, JSON fallback and serialization', () => {
    for (const value of ['', 'null', '1', '"text"', '{"a":1}', '[', 'a§b', '[1,null,{"x":"::"},[2,3]]',
      '["a\\u003a\\u003ab","\\ud800","\\n"]', 'x::y:z', '{{random::a::b}}']) {
      for (const name of ['makearray', 'array', 'a', 'arrayshift', 'arraypop', 'arraypush', 'arraysplice']) {
        for (const separator of [':', '::']) {
          const text = '{{' + name + separator + '{{getvar::value}}' + separator + '-1' + separator + '1' + separator + 'x}}'
          const actual = evaluate(text, { value })
          if (actual) expect(actual.data, text + ':' + value).toBe(risuChatParser(text, { chatVariables: { value } }))
        }
      }
    }
    expect(evaluate('{{arraypush::{{getvar::value}}::end}}', { value: '1§2' }))
      .toEqual({ data: '["1","2","end"]', reads: ['value'] })
    expect(evaluate('{{makearray::{{getvar::value}}}}', { value: 'a'.repeat(16_384) })).toBeNull()
    expect(compileDisplayCbs('{{arrayassert::[]::1000000000::x}}')).toBeNull()
  })

  test('hands missing-argument errors and assembled arithmetic reads back to the original parser', () => {
    for (const source of ['{{trim}}', '{{split}}', '{{join}}', '{{calc}}', '{{arrayelement}}']) {
      expect(evaluate(source), source).toBeNull()
      expect(risuChatParser(source), source).toBe(source)
    }
    for (const [source, expected] of [['{{? $(1)}}', '1'], ['{{calc::@(1)}}', '0']]) {
      expect(evaluate(source), source).toBeNull()
      expect(risuChatParser(source), source).toBe(expected)
    }
    expect(evaluate('{{calc::{{getvar::expression}}}}', { expression: '$1+2' })).toBeNull()
    expect(compileDisplayCbs('{{? $1+2}}')).toBeNull()
    expect(compileDisplayCbs('{{calc::@1+2}}')).toBeNull()
  })

  test('bounds callback expansion, repeated replacement substitutions and intermediate text', () => {
    const replacement = "$$'".repeat(500)
    expect(evaluate('{{replace::' + 'a'.repeat(100) + '::a::' + replacement + '}}')).toBeNull()
    expect(evaluate('{{join::' + 'a§'.repeat(100) + 'a::' + 'b'.repeat(2000) + '}}')).toBeNull()
    expect(evaluate('{{length::' + 'a'.repeat(16_384) + '}}')).toBeNull()
    expect(evaluate('{{? ' + '1+'.repeat(2048) + '1}}')).toBeNull()
    const source = '{{#if 1}}' + '{{getvar::value}}'.repeat(4) + '{{/}}'
    expect(evaluate(source, { value: 'a'.repeat(70_000) })).toBeNull()
    expect(evaluate('{{replace::aba::a::$&}}')).toEqual({ data: 'aba', reads: [] })
  })

  test('reports actual reads including discarded when branches but excludes false if bodies', () => {
    const source = '{{#if 0}}{{getvar::skipped}}{{/if}}'
      + '{{#when 0}}{{getvar::discarded}}{{:else}}{{getvar::shown}}{{/when}}'
    expect(evaluate(source, { skipped: 'x', discarded: 'y', shown: 'z' })).toEqual({
      data: 'z', reads: ['discarded', 'shown'],
    })
  })

  test('preserves generated text until the next checkpoint and rejects changed spans', () => {
    const source = '{{getvar::value}}'
    const plan = createDisplayCbsPlan([source])!
    const executor = new DisplayCbsExecutor(plan)
    const context = { variables: { value: '{{random::a::b}}' } }
    expect(executor.evaluate(source, context)).toEqual({ data: '{{random::a::b}}', reads: ['value'] })
    for (const checkpoint of ['{{random::a::b}}', '{{getvar::other}}', '<user>', '{#if 1\nyes#}', '{{' + source]) {
      expect(executor.evaluate(checkpoint, context)).toBeNull()
    }
    expect(executor.evaluate(source, { variables: {} })).toBeNull()
  })

  test('rejects effects, dynamic names and keys, malformed blocks and deep parser boundaries', () => {
    for (const source of [
      '{{random::a::b}}', '{{settempvar::x::1}}', '{{gettempvar::x}}',
      '{{getvar::{{getvar::key}}}}', '{{{{getvar::name}}::x}}',
      '{{#if 0}}{{call::hidden}}{{/if}}', '{{#each a as x}}x{{/each}}',
      '{{unknown::{{getvar::value}}}}', '{{equal::a', '{{equal::a#}b}}', '{{#if 1}}x',
      '{{#if 1}}x{{/wrong}}', '<char>', '{#if 1\nx#}',
      '{{equal::'.repeat(512) + 'x' + '::x}}'.repeat(512),
    ]) expect(compileDisplayCbs(source)).toBeNull()
  })

  test('bounds reusable syntax by source units, nodes and nesting rather than context values', () => {
    expect(compileDisplayCbs('{{not::1}}', { sourceUnits: 5, nodes: 10, depth: 10 })).toBeNull()
    expect(compileDisplayCbs('{{not::{{not::1}}}}', { sourceUnits: 100, nodes: 1, depth: 10 })).toBeNull()
    expect(compileDisplayCbs('{{not::{{not::1}}}}', { sourceUnits: 100, nodes: 10, depth: 1 })).toBeNull()
    const plan = createDisplayCbsPlan(['{{getvar::a}}', '{{getvar::b}}'], { sourceUnits: 100, nodes: 1, depth: 10 })!
    const executor = new DisplayCbsExecutor(plan)
    expect(executor.evaluate('{{getvar::a}}', { variables: { a: 'first' } })?.data).toBe('first')
    expect(executor.evaluate('{{getvar::a}}', { variables: { a: 'changed' } })?.data).toBe('changed')
    expect(executor.evaluate('{{getvar::b}}', { variables: { b: 'excluded' } })).toBeNull()
  })
})

test('compiled Worker checkpoints preserve the original parser outputs and random order across fallback', async () => {
  const { getRegexExecutionPlan, executeRegexPlanSync } = await import('../../../process/regexExecutionPlan')
  const { RegexExecutor } = await import('../../../process/regexExecutor')
  const { RegexWorkerClient } = await import('../../../process/regexWorkerClient')
  const { InMemoryWorker } = await import('../../../../../tests/support/regexWorker')
  const fixtures: Array<{ input: string, variables: Record<string, string>, rules: [string, string][], hostParses?: number, requests?: number, expectedData?: string }> = [
    ...['hello {{unknownmacro}}', '```\n{{ x }}\n```', 'hello <user>'].map(input => ({
      input, variables: { value: 'unused' },
      rules: [['missing', '{{getvar::value}}'], ...Array.from({ length: 7 }, () => ['absent', 'unused'])] as [string, string][],
      hostParses: input.includes('<user>') ? 1 : 8, requests: 2,
      expectedData: input.includes('<user>') ? 'hello Synthetic' : input,
    })),
    { input: 'x', variables: { value: '{{random::left::right}}', finish: 'done' },
      rules: [['x', '{{getvar::value}}'], ['absent', 'unused'], ['left|right', '{{getvar::finish}}']] },
    { input: 'x', variables: { finish: 'done' },
      rules: [['x', '{{settempvar::t::1}}{{random::left::right}}{{gettempvar::t}}'], ['left1|right1', '{{getvar::finish}}']] },
    { input: 'x', variables: { finish: 'done' },
      rules: [['x', '{{unknown::{{random::left::right}}}}'], ['missing', '{{getvar::finish}}']] },
    { input: 'x', variables: { expression: '$(1)', finish: 'done' },
      rules: [['x', '{{random::left::right}}'], ['left|right', '{{calc::{{getvar::expression}}}}'], ['1', '{{upper::{{getvar::finish}}}}']] },
    { input: 'x', variables: { value: '{{random::left::right}}' },
      rules: [['x', '{{trim::{{getvar::value}}}}'], ['absent', 'unused'], ['left|right', '{{join::[1,2]::+}}']] },
    { input: 'x', variables: {},
      rules: [['x', '{{button::{{arrayelement::["left","right"]::1}}::{{calc::2+3}}}}']] },
    { input: 'x', variables: { value: '{{random::left::right}}' },
      rules: [['x', '{{arraypush::[]::{{getvar::value}}}}'], ['absent', 'unused'], ['left|right', '{{arrayshift::[1,2]}}']] },
    { input: 'x', variables: {},
      rules: [['x', '{{arraysplice::{{makearray::a::b::c}}::-1::1}}'], ['absent', '{{arraypop::[1,2]}}']] },
    { input: 'value=a::a', variables: {},
      rules: [['value=(.*)', '{{equal::$1::a}}']] },
    { input: 'x{random::left::right}}', variables: { finish: 'done' },
      rules: [['x', '{'], ['left|right', '{{getvar::finish}}']] },
  ]
  for (const fixture of fixtures) {
    const plan = getRegexExecutionPlan(fixture.rules.map(([pattern, replacement]) => ({
      comment: '', type: 'editdisplay', in: pattern, out: replacement, flag: 'g', ableFlag: true,
    })), 'editdisplay')
    let hostParses = 0
    const parse = (value: string) => { hostParses++; return risuChatParser(value, { chatVariables: fixture.variables, userName: 'Synthetic' }) }
    const random = vi.spyOn(Math, 'random').mockReturnValue(0.25)
    try {
      const expected = executeRegexPlanSync(plan, fixture.input, parse)
      const expectedDraws = random.mock.calls.length
      random.mockClear()
      hostParses = 0
      const worker = new InMemoryWorker()
      const executor = new RegexExecutor({ worker: true, skipPlainCbs: true,
        client: new RegexWorkerClient(() => worker),
        displayCbs: { project: () => ({ variables: fixture.variables }), record() {} },
      })
      const actual = await executor.execute(plan, fixture.input, parse, async () => {
        throw new Error('These fixtures have no host regex actions')
      })
      expect(actual.data).toBe(expected.data)
      expect(random.mock.calls.length).toBe(expectedDraws)
      if (fixture.hostParses !== undefined) {
        expect(actual.data).toBe(fixture.expectedData)
        expect(hostParses).toBe(fixture.hostParses)
        expect(worker.requests.filter(request => request.type === 'execute')).toHaveLength(fixture.requests!)
      }
    } finally { random.mockRestore() }
  }
})

test('bounds projected values and dependency keys before sending a Worker request', () => {
  expect(projectDisplayCbsContext(['x'], () => '1234', 5)).toEqual({ variables: { x: '1234' } })
  expect(projectDisplayCbsContext(['x'], () => '12345', 5)).toBeUndefined()
  expect(projectDisplayCbsContext(Array.from({ length: 257 }, (_, index) => String(index)), () => '')).toBeUndefined()
  const sources = ['{{getvar::a}}', '{{getvar::b}}', '{{equal::{{getvar::a}}::kept}}']
  const plan = createDisplayCbsPlan([...sources, ...sources], { sourceUnits: 100, nodes: 10, depth: 10, variables: 1 })!
  expect(projectDisplayCbsContext(plan.keys, (key) => key === 'a' ? 'kept' : 'excluded'))
    .toEqual({ variables: { a: 'kept' } })
  const executor = new DisplayCbsExecutor(plan)
  expect(executor.evaluate('{{getvar::a}}', { variables: { a: 'kept' } })?.data).toBe('kept')
  expect(executor.evaluate('{{getvar::b}}', { variables: { b: 'excluded' } })).toBeNull()
  expect(executor.evaluate(sources[2], { variables: { a: 'kept' } })).toEqual({ data: '1', reads: ['a'] })
})
