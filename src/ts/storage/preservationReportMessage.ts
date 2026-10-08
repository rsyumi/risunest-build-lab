import { language } from 'src/lang'
import { formatBytes, fillTemplate } from '../gui/nativeFileJobDialogModel'
import type { NativeFileJobStatus } from './nativeFileJobs'

export type PreservationReport = NonNullable<NativeFileJobStatus['preservationReport']>

/** The notice shown after a restore kept source files that the restored library does not need. */
export function formatPreservationReport(report: PreservationReport): string {
    const strings = language.portableBackup
    return [
        fillTemplate(strings.preservedSummary, Number(report.files).toLocaleString(), formatBytes(Number(report.bytes))),
        strings.preservedSourceHelp,
        report.path,
    ].join('\n')
}
