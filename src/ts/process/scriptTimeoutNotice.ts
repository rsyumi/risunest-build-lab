import { alertToast } from '../alert'
import { language } from '../../lang'

let displayRuns = 0
let displayNoticeShown = false

/**
 * Marks a display listener run. Rows of one display refresh run back to back, so a
 * timeout notice is shown once until every queued display run has ended.
 */
export function beginDisplayScriptRun(): () => void {
    displayRuns++
    let ended = false
    return () => {
        if (ended) return
        ended = true
        displayRuns--
        if (displayRuns === 0) displayNoticeShown = false
    }
}

/** Tells the user that the time limit stopped a Lua script. */
export function noticeScriptTimeout(display: boolean): void {
    if (display && displayRuns > 0) {
        if (displayNoticeShown) return
        displayNoticeShown = true
    }
    alertToast(language.risuNest.scriptTimeout)
}
