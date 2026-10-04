import { expect, it } from 'vitest'
import { languageEnglish } from './en'
import { languageKorean } from './ko'
import { languageChinese } from './cn'
import { languageChineseTraditional } from './zh-Hant'
import { languageGerman } from './de'
import { languageSpanish } from './es'
import { languageVietnamese } from './vi'

it('provides all binding, restore and status copy in every language', () => {
    for (const language of [languageKorean, languageChinese, languageChineseTraditional, languageGerman, languageSpanish, languageVietnamese]) {
        expect(Object.keys(language.lwwSync).sort()).toEqual(Object.keys(languageEnglish.lwwSync).sort())
        for (const text of Object.values(language.lwwSync)) expect(text.length).toBeGreaterThan(0)
    }
})
it('preserves the exact approved Korean replacement and restore copy', () => {
    expect(languageKorean.lwwSync).toEqual({
        concurrentEditNotice: '같은 항목을 여러 기기에서 동시에 수정할 경우 마지막 수정 내용이 유지됩니다. 수정한 내용이나 메시지가 사라질 수 있으니 여러 기기를 동시에 사용하지 마세요.',
        replaceTitle: '이 기기의 데이터를 교체하시겠습니까?',
        replaceDescription: '동기화를 위해 이 기기의 데이터를 초기화한 후 원격 데이터로 교체합니다. 백업이 필요한 경우 수동으로 백업해주세요.',
        serverRestoredDescription: '서버가 백업에서 복원되었습니다. 이 기기의 데이터를 초기화한 후 서버 데이터로 교체하며, 백업 이후 이 기기에서 변경한 내용은 사라집니다. 백업이 필요한 경우 수동으로 백업해주세요.',
        replaceAcknowledge: '이 기기의 데이터 초기화', replaceAction: '교체', cancelAction: '취소',
        clockBlocked: '기기와 원격 간의 시간 차이가 있어 동기화가 중단되었습니다. 시간을 보정한 후 다시 시도해주세요.',
        writerCollision: '중복된 기기로 인해 동기화가 중단되었습니다. 새 기기로 다시 연결해주세요.',
        unitTooLarge: '이 기기에 서버로 보내기에 너무 큰 항목이 있어 동기화가 중단되었습니다. 해당 항목의 크기를 줄인 후 다시 시도해주세요.',
        newDeviceAction: '새 기기로 연결',
        restoreTitle: '백업을 복원하시겠습니까?',
        restoreDescriptionBound: '현재 데이터를 초기화한 후 선택한 백업으로 복원하며, 복원한 내용은 원격으로 동기화됩니다. 백업이 필요한 경우 수동으로 백업해주세요.',
        restoreDescription: '현재 데이터를 초기화한 후 선택한 백업으로 복원합니다. 백업이 필요한 경우 수동으로 백업해주세요.',
        restoreAcknowledge: '현재 데이터 초기화', restoreAction: '복원',
        previousFilesTitle: '연결하시겠습니까?',
        previousFilesDescription: '이전에 연결한 서버나 외부 저장소에만 있는 파일이 있습니다. 연결하면 이 파일을 새 연결 대상에도 저장하며, 이 기기에도 보관하려면 다운로드 후 연결을 선택하세요.',
        downloadThenConnect: '다운로드 후 연결',
        downloadFailedNotConnected: '파일을 다운로드하지 못해 연결하지 않았습니다. 다시 시도하거나 다운로드하지 않고 연결하세요.',
        previousStorageUnavailable: '이전에 연결한 서버나 외부 저장소에만 있는 파일을 가져오지 못해 동기화하지 못했습니다. 연결할 수 있을 때 다시 시도하세요.',
        registrationRevoked: '서버에서 이 기기의 등록이 해제되어 동기화할 수 없습니다. 서버에서 이 기기를 새로 등록한 뒤 새 등록 코드를 입력하세요.',
        bindingIncomplete: '연결이 완료되지 않았습니다. 연결하고 동기화를 눌러 연결을 마치세요.',
    })
})
it('preserves the exact approved English revoked-registration and unfinished-connection copy', () => {
    expect(languageEnglish.lwwSync.registrationRevoked).toBe("This device's registration was removed on the server, so it cannot sync. Register this device again on the server, then enter the new registration code.")
    expect(languageEnglish.lwwSync.bindingIncomplete).toBe('The connection was not completed. Press Connect and sync to finish it.')
})
it('preserves the exact approved English new-device action', () => {
    expect(languageEnglish.lwwSync.newDeviceAction).toBe('Connect as new device')
})
it('preserves the exact approved restored-server replacement copy', () => {
    expect(languageEnglish.lwwSync.serverRestoredDescription).toBe('The server was restored from a backup. The data on this device will be cleared and replaced with the server data, and changes made on this device after the backup will be lost. Make a manual backup if needed.')
    expect(languageChinese.lwwSync.serverRestoredDescription).toBe('服务器已从备份恢复。将清除此设备的数据并替换为服务器数据，备份之后在此设备上所做的修改将会丢失。如需备份，请手动备份。')
    expect(languageChineseTraditional.lwwSync.serverRestoredDescription).toBe('伺服器已從備份還原。將清除此裝置的資料並取代為伺服器資料，備份之後在此裝置上所做的修改將會遺失。如需備份，請手動備份。')
    expect(languageGerman.lwwSync.serverRestoredDescription).toBe('Der Server wurde aus einer Sicherung wiederhergestellt. Die Daten auf diesem Gerät werden gelöscht und durch die Serverdaten ersetzt. Änderungen, die nach der Sicherung auf diesem Gerät vorgenommen wurden, gehen verloren. Erstellen Sie bei Bedarf manuell eine Sicherung.')
    expect(languageSpanish.lwwSync.serverRestoredDescription).toBe('El servidor se restauró desde una copia de seguridad. Se borrarán los datos de este dispositivo y se reemplazarán por los datos del servidor, y se perderán los cambios hechos en este dispositivo después de la copia. Si necesita una copia de seguridad, créela manualmente.')
    expect(languageVietnamese.lwwSync.serverRestoredDescription).toBe('Máy chủ đã được khôi phục từ bản sao lưu. Dữ liệu trên thiết bị này sẽ bị xóa và thay thế bằng dữ liệu máy chủ, các thay đổi trên thiết bị này sau thời điểm sao lưu sẽ bị mất. Hãy sao lưu thủ công nếu cần.')
})
