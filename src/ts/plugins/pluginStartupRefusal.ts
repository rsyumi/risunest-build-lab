import { language } from '../../lang'
import { alertError } from '../alert'
import { pluginStartupGate } from './pluginStartupGate'

/** Refuses a generation the user started while plugins wait, and reports it in an error dialog. */
export function refuseGenerationWhilePluginsWait(): boolean {
    if (!pluginStartupGate.refuse()) return false
    alertError(language.risuNest.pluginStartup.requestRefused)
    return true
}
