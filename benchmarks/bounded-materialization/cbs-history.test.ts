import { expect, it } from 'vitest'
import { planChatParserHistory } from '../../src/ts/chatParserHistory'
import { prepareCbsHistory, type CbsHistoryReader } from '../../src/ts/prepareCbsHistory'
import type { Message } from '../../src/ts/storage/database.svelte'

it('measures CBS body transfer independently of logical conversation size', async () => {
    const measurements: Record<string, string | number>[] = []
    for (const total of [1_000, 10_000, 100_000]) {
        for (const bodyBytes of [256, 8_192]) {
            for (const backend of ['projected', 'materialized']) {
                for (const [kind, source] of Object.entries({
                    count: '{{lastmessageindex}}',
                    distant: `{{previouschatlog::2}}|{{previouschatlog::${total - 3}}}`,
                    tail: '{{lastmessage}}', previous: '{{previouscharchat}}',
                })) {
                    const resident = new Map<number, Message>()
                    let bodies = 0, roles = 0, maxPage = 0
                    const reader: CbsHistoryReader = {
                        revision: 1,
                        readResidentMessage: (index) => resident.get(index),
                        async readConversationWindow(query) {
                            const start = query.startIndex!, end = start + query.limit!
                            bodies += end - start
                            maxPage = Math.max(maxPage, end - start)
                            return { revision: 1, value: { characterId: 'c', conversationId: 'chat',
                                startIndex: start, endIndex: end, totalMessages: total,
                                hasMoreBefore: start > 0, hasMoreAfter: end < total,
                                messages: Array.from({ length: end - start }, (_, i) => ({
                                    role: (start + i) % 2 ? 'char' : 'user', data: 'x'.repeat(bodyBytes),
                                })) } }
                        },
                    }
                    if (backend === 'projected') reader.readRoleWindow = async (query) => {
                        const start = query.startIndex!, end = start + query.limit!
                        roles += end - start
                        return { revision: 1, value: { characterId: 'c', conversationId: 'chat',
                            startIndex: start, endIndex: end, totalMessages: total,
                            hasMoreBefore: start > 0, hasMoreAfter: end < total,
                            messages: Array.from({ length: end - start }, (_, i) => ({
                                role: (start + i) % 2 ? 'char' : 'user',
                                parserInert: true,
                            })) } }
                    }
                    for (const temperature of ['cold', 'resident']) {
                        bodies = roles = maxPage = 0
                        const started = performance.now()
                        const access = await prepareCbsHistory({ characterId: 'c', conversationId: 'chat',
                            totalMessages: total, chatID: -1, plan: planChatParserHistory({ source }), reader })
                        const elapsedMs = performance.now() - started
                        access.assertComplete()
                        if (kind === 'tail') expect(access.row(total - 1)?.data.length).toBe(bodyBytes)
                        if (kind === 'distant') {
                            expect(access.row(2)?.data.length).toBe(bodyBytes)
                            expect(access.row(total - 3)?.data.length).toBe(bodyBytes)
                        }
                        if (kind === 'previous') expect(access.previous('char', total, 1)[0]?.data.length).toBe(bodyBytes)
                        for (let index = 0; index < total; index++) {
                            const row = access.peek(index)
                            if (row) resident.set(index, row)
                        }
                        expect(access.length).toBe(total)
                        expect(bodies).toBeLessThanOrEqual(kind === 'previous' && backend === 'materialized' ? 32 : 2)
                        if (kind === 'count' || temperature === 'resident') expect(bodies).toBe(0)
                        measurements.push({ total, bodyBytes, backend, kind, temperature,
                            bodies, roleRows: roles, bodyBytesTransferred: bodies * bodyBytes,
                            retainedRows: resident.size, maxBodyPage: maxPage, elapsedMs: +elapsedMs.toFixed(3) })
                    }
                }
            }
        }
    }
    process.stdout.write('CBS_HISTORY_MEASUREMENTS ' + JSON.stringify({ measurements }) + '\n')
})
