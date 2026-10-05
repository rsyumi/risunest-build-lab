import { downloadFile } from "../globalApi.svelte";
import { alertError, alertNormal } from "../alert";
import { language } from "src/lang";
import { materializePersistentDatabaseSnapshot } from "./persistentDataRuntime.svelte";
import { DATASET_EXPORT_FILE_NAME, exportNativeDataset } from "./nativeDatasetExportRoute";

let activeExport: Promise<void> | undefined

export function exportAsDataset(): Promise<void> {
    if (activeExport) return activeExport
    const operation = exportDataset().finally(() => { if (activeExport === operation) activeExport = undefined })
    activeExport = operation
    return operation
}

async function exportDataset(){
    try {
        if(await exportNativeDataset() !== undefined){
            return
        }
    } catch (error) {
        if(!(error instanceof DOMException && error.name === 'AbortError')){
            alertError(error as Error)
        }
        return
    }

    const db = await materializePersistentDatabaseSnapshot('dataset-export')

    let dataset = []
    for(const char of db.characters){

        for(const chat of char.chats){
            
            dataset.push({
                name: char.name,
                description: char.desc,
                chats: chat.message,
                lorebook: char.globalLore
            })
        }
    }

    await downloadFile(DATASET_EXPORT_FILE_NAME,Buffer.from(JSON.stringify(dataset, null,4), 'utf-8'))

    alertNormal(language.successExport)
    
}
