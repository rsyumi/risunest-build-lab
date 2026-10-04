import { downloadFile } from "../globalApi.svelte";
import { alertError, alertNormal } from "../alert";
import { language } from "src/lang";
import { materializePersistentDatabaseSnapshot } from "./persistentDataRuntime.svelte";
import { DATASET_EXPORT_FILE_NAME, exportNativeDataset } from "./nativeDatasetExportRoute";

export async function exportAsDataset(){
    try {
        if(await exportNativeDataset() !== undefined){
            alertNormal(language.successExport)
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
        if(char.type === 'group'){
            continue
        }
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
