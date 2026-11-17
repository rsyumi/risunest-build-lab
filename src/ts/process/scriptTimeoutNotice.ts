import { alertToast } from '../alert'
import { language } from '../../lang'
import { createRegexTimeoutScope, type RegexTimeoutScope } from './regexTimeoutScope'

let displayRuns = 0
let displayNoticeShown = false
let displayRegexScope: RegexTimeoutScope | undefined

export function getDisplayRegexTimeoutScope(): RegexTimeoutScope | undefined {
    return displayRegexScope
}

/**
 * Marks a display listener run. Rows of one display refresh run back to back, so a
 * timeout notice is shown once until every queued display run has ended.
 */
export function beginDisplayScriptRun(): () => void {
    if (displayRuns === 0) displayRegexScope = createRegexTimeoutScope()
    displayRuns++
    let ended = false
    return () => {
        if (ended) return
        ended = true
        displayRuns--
        if (displayRuns === 0) {
            displayNoticeShown = false
            displayRegexScope = undefined
        }
    }
}

export function noticeRegexTimeout(): void {
    alertToast(language.regexScriptTimeout)
}

/** Tells the user that the time limit stopped a Lua script. */
export function noticeScriptTimeout(display: boolean): void {
    if (display && displayRuns > 0) {
        if (displayNoticeShown) return
        displayNoticeShown = true
    }
    alertToast(language.risuNest.scriptTimeout)
}
