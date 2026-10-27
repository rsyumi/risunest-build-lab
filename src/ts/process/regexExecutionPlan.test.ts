import { describe, expect, it, vi } from 'vitest'
import type { customscript } from '../storage/database.svelte'
import { setRuntimePerformanceProfile } from '../runtimePerformanceProfile'
import { executeRegexPlanSync, getRegexExecutionPlan } from './regexExecutionPlan'

function script(
    input: string,
    output: string,
    flag = 'g',
    ableFlag = true,
): customscript {
    return {
        comment: '',
        in: input,
        out: output,
        type: 'editoutput',
        flag,
        ableFlag,
    }
}

const identity = (value: string) => value
const largeDisplayHtml = '{{#if 1}}<section>' + '<span class="value">{{trim::x}}</span>'.repeat(5000) + '</section>{{/}}'

describe('regex execution plans', () => {
    it('observes in-place edits, order and removal after reusing a plan', () => {
        const scripts = [script('a', 'b'), script('b', 'c')]
        const run = () => executeRegexPlanSync(getRegexExecutionPlan([...scripts], 'editoutput'), 'a A', identity).data
        expect(run()).toBe('c A')
        expect(run()).toBe('c A')
        scripts[0].out = 'x'
        expect(run()).toBe('x A')
        scripts[0].in = 'A'
        expect(run()).toBe('a x')
        scripts[0].flag = 'gi'
        expect(run()).toBe('x x')
        scripts[0].ableFlag = false
        expect(run()).toBe('a x')
        scripts[0].type = 'editdisplay'
        expect(run()).toBe('a A')
        scripts[0] = script('a', 'b')
        scripts.reverse()
        expect(run()).toBe('b A')
        scripts.pop()
        expect(run()).toBe('a A')
        scripts.push(script('a', 'tail'))
        expect(run()).toBe('tail A')
    })
    it('reuses a plan for the same ordered script content', () => {
        const scripts = [script('a', 'b')]

        const first = getRegexExecutionPlan(scripts, 'editoutput')
        const second = getRegexExecutionPlan(scripts.map((value) => ({ ...value })), 'editoutput')
        const changed = getRegexExecutionPlan([script('a', 'c')], 'editoutput')

        expect(second).toBe(first)
        expect(Number.isSafeInteger(first.revision)).toBe(true)
        expect(changed).not.toBe(first)
        expect(changed.revision).toBeGreaterThan(first.revision)
    })

    it('keeps HTML plans reusable across modes and reevaluates optional CBS after profile changes', () => {
        const html = largeDisplayHtml
        const scripts = [{ ...script('profile-cache', html), type: 'editdisplay' }, script('profile-output', 'changed')]
        let previousRevision: number | undefined
        try {
            for (const profile of ['normal', 'low-spec', 'normal', 'low-spec'] as const) {
                setRuntimePerformanceProfile(profile)
                const plan = getRegexExecutionPlan(scripts, 'editdisplay')
                if (previousRevision !== undefined) expect(plan.revision).toBeGreaterThan(previousRevision)
                expect(getRegexExecutionPlan(scripts, 'editdisplay').revision).toBe(plan.revision)
                for (let attempt = 0; attempt < 3; attempt++) {
                    getRegexExecutionPlan(scripts, 'editoutput')
                    expect(getRegexExecutionPlan(scripts, 'editdisplay').revision).toBe(plan.revision)
                }
                if (profile === 'normal') expect(plan.displayCbs).toBeDefined()
                else expect(plan.displayCbs).toBeUndefined()
                previousRevision = plan.revision
            }
        } finally { setRuntimePerformanceProfile('normal') }
    })

    it('observes edits and preserves ordered execution and errors without optional CBS', () => {
        setRuntimePerformanceProfile('low-spec')
        const scripts = [
            { ...script('start', largeDisplayHtml, 'g<order 3>'), type: 'editdisplay' },
            { ...script('x', 'y', 'g<order 1>'), type: 'editdisplay' },
            { ...script('[', 'broken', 'g<order 2>'), type: 'editdisplay' },
        ]
        try {
            const first = getRegexExecutionPlan(scripts, 'editdisplay')
            expect(first.displayCbs).toBeUndefined()
            getRegexExecutionPlan([], 'editoutput')
            const reused = getRegexExecutionPlan(scripts, 'editdisplay')
            expect(reused.revision).toBe(first.revision)
            const result = executeRegexPlanSync(reused, 'start', identity)
            expect(result.data).toBe(largeDisplayHtml.replaceAll('x', 'y'))
            expect(result.errors.map((error) => error.sourceIndex)).toEqual([2])

            scripts[0].out = largeDisplayHtml.replaceAll('x', 'z')
            scripts.reverse()
            const changed = getRegexExecutionPlan(scripts, 'editdisplay')
            expect(changed.revision).toBeGreaterThan(first.revision)
            getRegexExecutionPlan([], 'editoutput')
            expect(getRegexExecutionPlan(scripts, 'editdisplay').revision).toBe(changed.revision)
            const changedResult = executeRegexPlanSync(changed, 'start', identity)
            expect(changedResult.data).toBe(largeDisplayHtml.replaceAll('x', 'z'))
            expect(changedResult.errors.map((error) => error.sourceIndex)).toEqual([0])
        } finally { setRuntimePerformanceProfile('normal') }
    })

    it('applies descending order metadata with stable ties', () => {
        const plan = getRegexExecutionPlan([
            script('a', 'b', 'g<order 1>'),
            script('b', 'c', 'g<order 2>'),
            script('b', 'd', 'g<order 1>'),
        ], 'editoutput')

        expect(executeRegexPlanSync(plan, 'a', identity).data).toBe('d')
    })

    it('uses default flags when flags are disabled', () => {
        const plan = getRegexExecutionPlan([
            script('A', 'x', 'i', false),
        ], 'editoutput')

        expect(executeRegexPlanSync(plan, 'a A', identity).data).toBe('a x')
    })

    it('normalizes supported flags and removes duplicates', () => {
        const plan = getRegexExecutionPlan([
            script('a', 'x', ' gggi!! '),
        ], 'editoutput')

        expect(executeRegexPlanSync(plan, 'a A', identity).data).toBe('x x')
    })

    it('falls back to unicode mode when no supported flag remains', () => {
        const plan = getRegexExecutionPlan([
            script('a', 'x', '!!!'),
        ], 'editoutput')

        expect(executeRegexPlanSync(plan, 'a a', identity).data).toBe('x a')
    })

    it('feeds each replacement through the parser before the next rule', () => {
        const plan = getRegexExecutionPlan([
            script('a', 'b'),
            script('B', 'c'),
        ], 'editoutput')

        expect(executeRegexPlanSync(plan, 'a', (value) => value.toUpperCase()).data).toBe('C')
    })

    it.each([
        {
            name: 'lookaround',
            rules: [script('(?<=foo)bar', 'baz')],
            input: 'foobar',
            expected: 'foobaz',
        },
        {
            name: 'numbered backreference replacement',
            rules: [script('(a)(b)', '$2$1')],
            input: 'ab',
            expected: 'ba',
        },
        {
            name: 'named capture replacement',
            rules: [script('(?<left>a)(?<right>b)', '$<right>$<left>')],
            input: 'ab',
            expected: 'ba\n',
        },
        {
            name: 'ECMAScript replacement tokens',
            rules: [script('(abc)', '[$&][$1][$`][$\']')],
            input: 'abc',
            expected: '[abc][abc][][]',
        },
    ])('preserves $name behavior', ({ rules, input, expected }) => {
        const plan = getRegexExecutionPlan(rules, 'editoutput')

        expect(executeRegexPlanSync(plan, input, identity).data).toBe(expected)
    })

    it.each([
        { flag: 'g', input: 'aa', expected: 'xx' },
        { flag: 'y', input: 'a', expected: 'x' },
    ])('resets $flag lastIndex before every execution', ({ flag, input, expected }) => {
        const plan = getRegexExecutionPlan([script('a', 'x', flag)], 'editoutput')

        expect(executeRegexPlanSync(plan, input, identity).data).toBe(expected)
        expect(executeRegexPlanSync(plan, input, identity).data).toBe(expected)
    })

    it('replaces at position 0 when a sticky rule with actions tests first', () => {
        const plan = getRegexExecutionPlan([
            script('foo', 'X', 'y<no_end_nl>'),
        ], 'editoutput')

        expect(executeRegexPlanSync(plan, 'foofoo', identity).data).toBe('Xfoo')
        expect(executeRegexPlanSync(plan, 'foofoo', identity).data).toBe('Xfoo')
    })

    it('isolates an invalid regex and continues with later rules', () => {
        const errorLog = vi.spyOn(console, 'error').mockImplementation(() => {})
        const plan = getRegexExecutionPlan([
            script('[', 'broken'),
            script('a', 'b'),
        ], 'editoutput')

        const result = executeRegexPlanSync(plan, 'a', identity)

        expect(result.data).toBe('b')
        expect(result.errors).toHaveLength(1)
        expect(result.errors[0].sourceIndex).toBe(0)
        errorLog.mockRestore()
    })

    it('reparses CBS patterns for every invocation', () => {
        const plan = getRegexExecutionPlan([
            script('<pattern>', 'b', 'g<cbs>'),
        ], 'editoutput')
        let patternParses = 0
        const parse = (value: string) => {
            if (value === '<pattern>') {
                patternParses++
                return 'a'
            }
            return value
        }

        expect(executeRegexPlanSync(plan, 'a', parse).data).toBe('b')
        expect(executeRegexPlanSync(plan, 'a', parse).data).toBe('b')
        expect(patternParses).toBe(2)
    })

    it('keeps a recently reused plan when the cache evicts', () => {
        const scriptSets = Array.from({ length: 32 }, (_value, index) => [script(`plan-${index}`, 'x')])
        const plans = scriptSets.map((set) => getRegexExecutionPlan(set, 'editoutput'))

        expect(getRegexExecutionPlan(scriptSets[0], 'editoutput')).toBe(plans[0])
        getRegexExecutionPlan([script('plan-extra', 'x')], 'editoutput')

        expect(getRegexExecutionPlan(scriptSets[0], 'editoutput')).toBe(plans[0])
        expect(getRegexExecutionPlan(scriptSets[1], 'editoutput')).not.toBe(plans[1])
    })

    it('rejects stateful actions from worker eligibility', () => {
        const directivePlan = getRegexExecutionPlan([
            script('a', '@@emo happy'),
        ], 'editoutput')
        const actionPlan = getRegexExecutionPlan([
            script('a', 'b', 'g<inject>'),
        ], 'editoutput')
        const orderedReplacementPlan = getRegexExecutionPlan([
            script('a', 'b', 'g<order 1>'),
        ], 'editoutput')

        expect(directivePlan.workerEligible).toBe(false)
        expect(actionPlan.workerEligible).toBe(false)
        expect(orderedReplacementPlan.workerEligible).toBe(true)
    })
})
