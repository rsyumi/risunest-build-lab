import { readdirSync, readFileSync } from 'node:fs'
import { join } from 'node:path'
import { describe, expect, it } from 'vitest'
import { externalStorageStrings } from '../lib/Setting/ExternalStorage/strings'
import { languageChinese } from './cn'
import { languageGerman } from './de'
import { languageEnglish } from './en'
import { languageSpanish } from './es'
import { languageKorean } from './ko'
import { languageVietnamese } from './vi'
import { languageChineseTraditional } from './zh-Hant'

function asksAgain(text: string): boolean {
    return text.includes('?') || ['No를', '정말', 'Really'].some((question) => text.includes(question))
}

function sourceFiles(directory: string): string[] {
    return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
        const path = join(directory, entry.name).replaceAll('\\', '/')
        if (entry.isDirectory()) return sourceFiles(path)
        return /\.(ts|svelte)$/.test(entry.name) && !/\.test\.|\.d\.ts$/.test(entry.name) ? [path] : []
    })
}

function resolvePath(root: unknown, path: string[]): unknown {
    let value = root
    for (const key of path) value = value && typeof value === 'object' ? (value as Record<string, unknown>)[key] : undefined
    // A composed message is checked in the form its call site uses, without the trailing question.
    return typeof value === 'function' ? value('Synthetic', 1, 1, false) : value
}

/** The texts each `alertCheckboxConfirm` description can show, by call site. */
function checkboxDescriptions() {
    const sites: { site: string; texts: string[] }[] = []
    for (const file of sourceFiles('src')) {
        const source = readFileSync(file, 'utf8')
        const aliases = new Map<string, (locale: 'ko' | 'en') => unknown>([
            ['language', (locale) => locale === 'ko' ? languageKorean : languageEnglish],
        ])
        for (const match of source.matchAll(/const (\w+) = language((?:\.\w+)+)/g)) {
            const path = match[2].slice(1).split('.')
            aliases.set(match[1], (locale) => resolvePath(locale === 'ko' ? languageKorean : languageEnglish, path))
        }
        for (const match of source.matchAll(/const (\w+) = \$derived\(externalStorageStrings\(/g)) {
            aliases.set(match[1], (locale) => externalStorageStrings(locale))
        }
        for (const call of source.matchAll(/alertCheckboxConfirm\(\{/g)) {
            const start = call.index!
            const end = source.indexOf('checkboxLabel', start)
            const segment = source.slice(source.indexOf('description', start) + 'description'.length, end)
            let expression = segment.trimStart().startsWith(':') ? segment : 'description'
            for (const name of new Set(expression.match(/\b[A-Za-z_]\w*\b/g) ?? [])) {
                if (aliases.has(name)) continue
                const declaration = source.lastIndexOf(`const ${name} = `, start)
                if (declaration >= 0) expression += source.slice(declaration, start)
            }
            const texts: string[] = []
            for (const reference of expression.matchAll(/\b(\w+)((?:\.\w+)+)/g)) {
                const root = aliases.get(reference[1])
                if (!root) continue
                for (const locale of ['ko', 'en'] as const) {
                    const text = resolvePath(root(locale), reference[2].slice(1).split('.'))
                    if (typeof text === 'string') texts.push(text)
                }
            }
            sites.push({ site: `${file}:${source.slice(0, start).split('\n').length}`, texts })
        }
    }
    return sites
}

describe('checkbox confirmation copy', () => {
    it('offers later-message deletion as an opt-in and states what removal does', () => {
        expect(languageKorean.checkboxConfirmation.laterMessageDeletion).toBe('이후 메시지도 함께 삭제')
        expect(languageEnglish.checkboxConfirmation.laterMessageDeletion).toBe('Also delete later messages')
        expect(languageKorean.checkboxConfirmation.messageDeletionDescription).toBe(
            '이 메시지만 삭제되며, 이후 메시지도 함께 삭제할 경우 이 메시지 뒤의 모든 메시지가 함께 삭제됩니다.',
        )
        expect(languageEnglish.checkboxConfirmation.messageDeletionDescription).toBe(
            'Only this message is deleted. If you also delete later messages, every message after it is deleted too.',
        )
        expect(languageKorean.remove).toBe('삭제')
        for (const language of [
            languageKorean, languageEnglish, languageChinese, languageGerman, languageSpanish, languageVietnamese, languageChineseTraditional,
        ]) {
            expect(language.checkboxConfirmation as Record<string, unknown>).not.toHaveProperty('onlySelectedMessage')
        }
    })

    it('states each consequence instead of asking again', () => {
        expect(languageKorean.checkboxConfirmation).toMatchObject({
            hypaResetDescription: '이 채팅의 HypaV3 데이터가 모두 초기화되며, 되돌릴 수 없습니다.',
            hypaDeletionDescription: '이 요약 뒤의 모든 요약이 삭제되며, 되돌릴 수 없습니다.',
            characterTrashDescription: '이 캐릭터가 휴지통으로 이동하며, 휴지통에서 복구할 수 있습니다.',
            characterDeletionDescription: '이 캐릭터와 모든 채팅이 영구적으로 삭제되며, 되돌릴 수 없습니다.',
            lorebookDeletionDescription: '이 폴더와 폴더 안의 모든 로어북이 삭제됩니다.',
            dataReplacementDescription: '현재 데이터가 백업의 데이터로 교체되며, 자동 백업은 만들지 않습니다. 기존 데이터를 보관하려면 취소 후 직접 백업하세요.',
            partialBackupTitle: '부분 로컬 백업을 저장하시겠습니까?',
            partialBackupDescription: '이 백업에는 데이터베이스와 캐릭터 프로필 이미지, 사용자 아이콘, 커스텀 배경, 페르소나 아이콘, 폴더 이미지, 봇 프리셋 이미지만 포함되며, 감정 이미지, 추가 캐릭터 에셋, VITS 음성 파일 등 그 밖의 에셋은 포함되지 않습니다.',
        })
        expect(languageEnglish.checkboxConfirmation).toMatchObject({
            hypaResetDescription: 'All HypaV3 data in this chat is reset. This cannot be undone.',
            hypaDeletionDescription: 'Every summary after this one is deleted. This cannot be undone.',
            characterTrashDescription: 'This character moves to the trash, where you can restore it.',
            characterDeletionDescription: 'This character and all of its chats are permanently deleted. This cannot be undone.',
            lorebookDeletionDescription: 'This folder and every lorebook in it are deleted.',
            dataReplacementDescription: 'Your current data is replaced by the backup, and no automatic backup is created. To keep your current data, cancel and back it up first.',
            partialBackupTitle: 'Save a partial local backup?',
            partialBackupDescription: 'This backup includes only the database and the character profile images, user icon, custom background, persona icons, folder images and bot preset images. Emotion images, additional character assets, VITS voice files and other assets are not included.',
        })
        expect(languageKorean.risuNest.pluginData).toMatchObject({
            deleteAllConfirmFinal: '목록의 모든 값이 삭제되며, 삭제한 값은 복원할 수 없습니다.',
            deleteVisibleConfirmFinal: '보이는 값이 모두 삭제되며, 삭제한 값은 복원할 수 없습니다.',
        })
        expect(languageEnglish.risuNest.pluginData).toMatchObject({
            deleteAllConfirmFinal: 'Every value in the list is deleted. Deleted values cannot be restored.',
            deleteVisibleConfirmFinal: 'Every value shown is deleted. Deleted values cannot be restored.',
        })
        expect(externalStorageStrings('ko')).toMatchObject({
            deleteOtherDeviceConfirm: '다른 기기에서 만든 백업입니다.',
            deleteLastRetainedConfirm: '마지막으로 보관된 백업 지점입니다.',
        })
        expect(externalStorageStrings('en')).toMatchObject({
            deleteOtherDeviceConfirm: 'This backup was made on another device.',
            deleteLastRetainedConfirm: 'This is the last retained backup point.',
        })
    })

    it('gives every checkbox dialog a statement for its description', () => {
        const sites = checkboxDescriptions()
        expect(sites.length).toBeGreaterThanOrEqual(15)
        const violations = sites.flatMap(({ site, texts }) => texts.length === 0
            ? [`${site}: no description found`]
            : texts.filter(asksAgain).map((text) => `${site}: ${text}`))
        expect(violations).toEqual([])
    })
})
